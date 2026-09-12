"""Generate the committed Python protobuf bindings for ``proto/altavista/v1``.

Run with the project's venv python::

    .venv/bin/python altavista/pb/generate.py

(or ``altavista/pb/generate.sh``, a thin wrapper around the same command).

What this does
---------------
Runs ``protoc`` with ``-I <repo>/proto`` over every ``proto/altavista/v1/*.proto`` file
and writes the generated ``*_pb2.py`` modules into ``altavista/pb/``, mirroring the proto
import paths (``import "altavista/v1/core.proto";``) as a real Python package tree:
``altavista/pb/altavista/v1/core_pb2.py``, etc. This is the same ``-I``/output shape
``tests/test_cdm_v1.py`` already uses for its ad-hoc generation into ``build/pb``
(kept there, untouched); the difference is that this output is committed and importable
as ``altavista.pb`` with no test-side ``sys.path`` hacking.

Why the nested layout
----------------------
protoc's Python codegen makes cross-file imports absolute and rooted at the proto
package path, e.g. ``entity_pb2.py`` is generated with ``from altavista.v1 import
core_pb2 as ...``. Flattening the output (generating with ``-I proto/altavista/v1`` so
files come out as bare ``core_pb2.py`` etc.) would break those cross-references, since
the .proto files themselves ``import "altavista/v1/core.proto"`` (dir-qualified). So the
nested ``altavista/v1/`` tree is kept intact under ``altavista/pb/``.

Rewriting protoc's absolute cross-imports to relative (M26.1)
---------------------------------------------------------------
Before the M26.1 rename (question 160), the containing Python package had a different
name, so protoc's absolute ``from altavista.v1 import X`` cross-imports were resolved by
adding ``altavista/pb/`` to ``sys.path`` once at import time, letting a bare top-level
``altavista`` module -- distinct from the differently-named containing package -- resolve
to the proto bindings. That trick breaks the moment the containing package is itself named
``altavista``: the bare name is then already bound in ``sys.modules`` to the *real* top
package by the time ``altavista.pb`` (a submodule reached from inside that package's own
``__init__.py`` chain) runs, so the ``sys.path`` entry is never consulted and
``altavista.v1`` resolves (wrongly, and only after the top package is fully bound) against
the real package's own ``__path__``, which has no ``v1`` -- ``ModuleNotFoundError``.
So ``_rewrite_cross_imports_relative`` below rewrites every generated ``from altavista.v1
import X as Y`` line (protoc always emits one absolute import per cross-referenced
sibling file, in every ``*_pb2.py``/``*_pb2_grpc.py``) to a plain relative ``from . import
X as Y`` immediately after protoc/``grpc_tools.protoc`` write it. Relative imports resolve
through the real package hierarchy the module was reached through, so no sys.path
manipulation is needed and no bare-name collision is possible, regardless of what the
containing package is called. ``altavista/pb/__init__.py`` reaches the seven ``*_pb2``
modules (and the grpc stub module) the same way, with a plain relative ``from
.altavista.v1 import (...)`` and no sys.path handling required by any importer (tests
included).

Determinism
-----------
protoc's Python output for a given input tree, ``-I`` root and flag set is
byte-identical across runs of the *same* protoc version (no embedded timestamps).
``PROTOC_VERSION`` below records the version this checked-in output was generated
with, so a drift (someone regenerating with a different protoc) is visible in the
diff and in the printed warning, rather than silently changing checked-in bytes.

gRPC stubs (``*_pb2_grpc.py``), M2.2
-------------------------------------
This script also runs ``grpc_tools.protoc`` (the ``grpcio-tools`` PyPI package, which
bundles its own ``protoc``) with ``--grpc_python_out`` only -- never ``--python_out`` --
so the ``*_pb2.py`` files above are always produced by the one system ``protoc`` this
module already shells out to; ``grpc_tools.protoc`` never gets a chance to regenerate
them with a possibly-different bundled protoc version. (Measured 2026-09-02: the
grpcio-tools 1.83.1 bundled protoc reports ``libprotoc 35.1`` -- the same as
``PROTOC_VERSION`` -- and produces byte-identical ``*_pb2.py`` output when tried; that
identity is not relied on, since a future grpcio-tools release could bundle a different
protoc.)

Only ``dynamics_service.proto`` declares a ``service``; running the gRPC plugin over
every ``*.proto`` file (asked for uniformly, like the ``*_pb2.py`` pass) still emits one
``*_pb2_grpc.py`` per input file, but every file without a ``service`` produces 24 lines
of pure boilerplate (a version-compatibility check, no stub/servicer classes) that would
just be noise in the committed tree. So generation runs into a scratch temp directory
and only the file(s) that actually declare a stub class are copied into ``OUT_DIR`` --
this is content-detected, not a hardcoded file list, so a future ``service`` in another
``proto/altavista/v1/*.proto`` file is picked up automatically on the next run.
"""
from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as _pkg_version
from pathlib import Path

# libprotoc version this checked-in output was generated with (see module docstring).
# Regenerating with a different protoc is not an error, but the diff (and the warning
# this script prints) makes the drift visible instead of silent.
PROTOC_VERSION = "libprotoc 35.1"

# grpcio-tools version this checked-in *_pb2_grpc.py was generated with (see module
# docstring). Its bundled protoc is used for --grpc_python_out only, never --python_out.
GRPC_TOOLS_VERSION = "1.83.1"

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
PROTO_DIR = REPO_ROOT / "proto"
OUT_DIR = Path(__file__).resolve().parent  # altavista/pb

# Proto files, in a fixed (sorted) order -- no directory-listing-order dependence.
PROTO_SRC_DIR = PROTO_DIR / "altavista" / "v1"

# Package __init__.py files this script must ensure exist (protoc does not write them).
PACKAGE_DIRS = [OUT_DIR, OUT_DIR / "altavista", OUT_DIR / "altavista" / "v1"]

def _build_pb_init_content(message_modules: list, grpc_modules: list) -> str:
    """Builds ``altavista/pb/__init__.py``'s content from what is actually on disk after
    this run -- ``message_modules`` (every ``*_pb2.py`` stem, e.g. ``"core_pb2"``) and
    ``grpc_modules`` (every ``*_pb2_grpc.py`` stem that already exists in ``OUT_DIR``,
    whether freshly regenerated this run or left over from an earlier one -- see
    ``generate()``'s own comment on why this is read from disk rather than only from this
    run's own ``_generate_grpc_stubs`` return value).

    Previously this was a static, hand-maintained string literal enumerating exactly the
    proto modules that existed when it was last edited by hand -- so a new
    ``proto/altavista/v1/*.proto`` file's ``*_pb2.py`` module was never exposed from this
    package's own ``__init__.py`` even after a fully successful regeneration, silently
    contradicting this module's own docstring ("a future service in another proto file is
    picked up automatically on the next run" -- true only of the gRPC-stub *content*
    detection, never of this list). Built dynamically instead, so a new proto file's
    module is exposed the moment its ``*_pb2.py`` exists, with no second, easily-forgotten
    hand-edit here.
    """
    import_lines = "".join(f"    {m},\n" for m in message_modules)
    grpc_try_blocks = "".join(
        f"try:\n    from .altavista.v1 import {g}\nexcept ImportError:  # grpcio not installed\n    {g} = None  # type: ignore[assignment]\n\n" for g in grpc_modules
    )
    all_entries = "".join(f'    "{m}",\n' for m in [*message_modules, *grpc_modules])
    grpc_comment = (
        "# The gRPC stub needs the optional ``grpcio`` extra (``pip install -e \".[grpc]\"``); the\n"
        "# messages above do not. Importing the package must not require grpcio (question 85), so\n"
        "# the stub is exposed as ``None`` when grpcio is absent and callers that need it check.\n"
        if grpc_modules
        else ""
    )
    return (
        '"""Committed Python protobuf bindings for proto/altavista/v1.\n\n'
        "Generated by altavista/pb/generate.py -- do not hand-edit the *_pb2.py files.\n"
        "See generate.py's module docstring for why the ``altavista/v1`` nesting is kept and\n"
        "why the generated modules' cross-imports are rewritten to relative (M26.1).\n"
        '"""\n'
        "from __future__ import annotations\n\n"
        "from .altavista.v1 import (\n"
        f"{import_lines}"
        ")\n\n"
        f"{grpc_comment}{grpc_try_blocks}"
        "__all__ = [\n"
        f"{all_entries}"
        "]\n"
    )


_CROSS_IMPORT_RE = re.compile(r"^from altavista\.v1 import (\w+) as (\w+)$", re.MULTILINE)


def _rewrite_cross_imports_relative(text: str) -> str:
    """Rewrite protoc's absolute sibling-file imports to relative ones (M26.1, see this
    module's docstring): ``from altavista.v1 import X as Y`` -> ``from . import X as Y``.
    Every generated ``*_pb2.py``/``*_pb2_grpc.py`` lives in this same directory
    (``altavista/pb/altavista/v1/``), so the relative form always resolves, through the
    real package hierarchy the module was reached through -- unlike the absolute form,
    which collides with the containing top-level package now that it is also named
    ``altavista``.
    """
    return _CROSS_IMPORT_RE.sub(r"from . import \1 as \2", text)


def _wkt_include_dir() -> list:
    """-I for google/protobuf/*.proto (any.proto etc), matching tests/test_cdm_v1.py."""
    for cand in ("/opt/homebrew/opt/protobuf/include", "/opt/homebrew/include", "/usr/local/include", "/usr/include"):
        if (Path(cand) / "google" / "protobuf" / "any.proto").exists():
            return ["-I", cand]
    return []


def _protoc() -> str:
    return shutil.which("protoc") or "/opt/homebrew/bin/protoc"


def _generate_grpc_stubs(files: list) -> list:
    """Run ``grpc_tools.protoc --grpc_python_out`` for every proto file, keep only the
    ones that actually declare a ``service`` (see module docstring), and copy those into
    ``OUT_DIR``. Returns the list of committed ``*_pb2_grpc.py`` paths (sorted).

    Uses ``--grpc_python_out`` only -- never ``--python_out`` -- so this never touches the
    ``*_pb2.py`` files the ``protoc`` pass above already wrote.

    ``grpcio-tools`` is declared as this repo's optional ``grpc``/``dev`` extra
    (``pyproject.toml``), not a hard dependency of this script -- mirroring
    ``PB_INIT_CONTENT``'s own tolerance of a ``grpcio``-less environment at import time
    (question 85). So a venv that lacks it (e.g. one where only the plain ``dev`` extra
    was installed, since ``pyproject.toml``'s ``dev`` extra pulls in ``grpcio`` but not
    ``grpcio-tools``) prints a clear warning and skips gRPC stub (re)generation entirely,
    rather than crashing the whole script and leaving even the unrelated ``*_pb2.py``
    message bindings above un-finalized (their own `__init__.py`/directory bookkeeping
    happens later in ``generate()``, after this function returns).
    """
    try:
        actual_version = _pkg_version("grpcio-tools")
    except PackageNotFoundError:
        print(
            "warning: grpcio-tools is not installed in this environment -- gRPC stubs "
            "(*_pb2_grpc.py) were NOT (re)generated this run; only the plain *_pb2.py "
            "message bindings were. Any *_pb2_grpc.py files already committed from an "
            "earlier run are left untouched on disk and still referenced from "
            "altavista/pb/__init__.py. Install grpcio-tools "
            f"(pinned version {GRPC_TOOLS_VERSION!r}) to regenerate gRPC stubs too.",
            file=sys.stderr,
        )
        return []
    if actual_version != GRPC_TOOLS_VERSION:
        print(
            f"warning: generating gRPC stubs with grpcio-tools {actual_version!r}, but the "
            f"committed *_pb2_grpc.py output was last generated with grpcio-tools "
            f"{GRPC_TOOLS_VERSION!r}. Output may not be byte-identical to what is checked "
            f"in; review the diff.",
            file=sys.stderr,
        )

    kept: list = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        cmd = [sys.executable, "-m", "grpc_tools.protoc", "-I", str(PROTO_DIR), *_wkt_include_dir(),
               f"--grpc_python_out={tmp_dir}", *files]
        subprocess.run(cmd, check=True)

        generated = sorted((tmp_dir / "altavista" / "v1").glob("*_pb2_grpc.py"))
        for g in generated:
            text = g.read_text()
            if "Stub" not in text:  # pure version-check boilerplate, no service declared
                continue
            dest = OUT_DIR / "altavista" / "v1" / g.name
            dest.write_text(_rewrite_cross_imports_relative(text))
            kept.append(dest)
    return kept


def generate() -> None:
    protoc = _protoc()
    if not Path(protoc).exists():
        raise FileNotFoundError(f"protoc not found at {protoc!r}; install it or set PATH")

    actual_version = subprocess.run([protoc, "--version"], check=True, capture_output=True, text=True).stdout.strip()
    if actual_version != PROTOC_VERSION:
        print(
            f"warning: generating with {actual_version!r}, but the committed output was last "
            f"generated with {PROTOC_VERSION!r}. Output may not be byte-identical to what is "
            f"checked in; review the diff.",
            file=sys.stderr,
        )

    files = sorted(str(p) for p in PROTO_SRC_DIR.glob("*.proto"))
    if not files:
        raise FileNotFoundError(f"no .proto files found under {PROTO_SRC_DIR}")

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    cmd = [protoc, "-I", str(PROTO_DIR), *_wkt_include_dir(), f"--python_out={OUT_DIR}", *files]
    subprocess.run(cmd, check=True)

    for g in (OUT_DIR / "altavista" / "v1").glob("*_pb2.py"):
        g.write_text(_rewrite_cross_imports_relative(g.read_text()))

    grpc_stubs = _generate_grpc_stubs(files)

    # Read back from disk, not from `grpc_stubs`/`files` above: a *_pb2_grpc.py already
    # committed from an earlier run (e.g. dynamics_service_pb2_grpc.py) must stay
    # referenced from altavista/pb/__init__.py even on a run where `_generate_grpc_stubs`
    # skipped regeneration entirely (grpcio-tools missing) -- see that function's own
    # doc comment.
    message_modules = sorted(p.stem for p in (OUT_DIR / "altavista" / "v1").glob("*_pb2.py"))
    grpc_modules = sorted(p.stem for p in (OUT_DIR / "altavista" / "v1").glob("*_pb2_grpc.py"))

    for d in PACKAGE_DIRS:
        init = d / "__init__.py"
        if d == OUT_DIR:
            init.write_text(_build_pb_init_content(message_modules, grpc_modules))
        elif not init.exists():
            init.write_text('"""Generated by altavista/pb/generate.py."""\n')

    generated = sorted((OUT_DIR / "altavista" / "v1").glob("*_pb2.py"))
    print(f"protoc: {actual_version}")
    print(f"generated {len(generated)} module(s) under {OUT_DIR / 'altavista' / 'v1'}:")
    for g in generated:
        print(f"  {g.relative_to(REPO_ROOT)}")
    try:
        grpc_tools_version = _pkg_version("grpcio-tools")
    except PackageNotFoundError:
        grpc_tools_version = "<not installed>"
    print(f"grpcio-tools: {grpc_tools_version}")
    print(f"generated {len(grpc_stubs)} gRPC stub module(s) under {OUT_DIR / 'altavista' / 'v1'}:")
    for g in grpc_stubs:
        print(f"  {g.relative_to(REPO_ROOT)}")
    if grpc_modules and not grpc_stubs:
        print(f"kept {len(grpc_modules)} previously-committed gRPC stub module(s) unchanged (not regenerated this run): {', '.join(grpc_modules)}")


if __name__ == "__main__":
    generate()
