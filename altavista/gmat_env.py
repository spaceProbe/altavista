"""Locate a GMAT installation and load its Python API (``gmatpy``).

Resolution order for the GMAT root folder (the folder containing ``bin/``):

1. Explicit ``root`` argument to :func:`load_gmat`.
2. ``GMAT_ROOT`` environment variable.
3. A folder named ``GMAT*`` next to this repository (e.g. ``AltaVista/GMAT R2026a``).

The GMAT API needs an *absolute-path* startup file (``bin/api_startup_file.txt``).
If it does not exist it is generated from ``bin/gmat_startup_file.txt`` the same way
GMAT's own ``api/BuildApiStartupFile.py`` does it.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Optional

_gmat_module = None
_gmat_root: Optional[Path] = None

REPO_ROOT = Path(__file__).resolve().parent.parent


def find_gmat_root(root: Optional[os.PathLike] = None) -> Path:
    """Return the GMAT install folder, raising ``FileNotFoundError`` if none is found."""
    candidates = []
    if root:
        candidates.append(Path(root))
    if os.environ.get("GMAT_ROOT"):
        candidates.append(Path(os.environ["GMAT_ROOT"]))
    candidates += sorted(REPO_ROOT.glob("GMAT*"), reverse=True)
    for c in candidates:
        c = c.expanduser()
        if (c / "bin" / "gmat_startup_file.txt").exists():
            return c.resolve()
    raise FileNotFoundError(
        "GMAT installation not found. Set GMAT_ROOT to the folder that contains bin/gmat_startup_file.txt"
    )


def ensure_api_startup_file(root: Path) -> Path:
    """Create ``bin/api_startup_file.txt`` (absolute paths) if it is missing."""
    src = root / "bin" / "gmat_startup_file.txt"
    dst = root / "bin" / "api_startup_file.txt"
    if not dst.exists() or dst.stat().st_mtime < src.stat().st_mtime:
        text = src.read_text()
        # GMAT's relative entries are written as "../data" etc. Replace ".." with the root.
        dst.write_text(text.replace("..", str(root)))
    return dst


def gmat_root() -> Path:
    """Root folder of the GMAT install that was (or will be) loaded."""
    global _gmat_root
    if _gmat_root is None:
        _gmat_root = find_gmat_root()
    return _gmat_root


def texture_dir() -> Path:
    return gmat_root() / "data" / "graphics" / "texture"


def load_gmat(root: Optional[os.PathLike] = None, log: bool = False):
    """Import and initialise ``gmatpy``. Safe to call repeatedly; returns the module.

    ``log=True`` echoes the GMAT log to stdout (useful when debugging a script).
    """
    global _gmat_module, _gmat_root
    if _gmat_module is not None:
        return _gmat_module
    _gmat_root = find_gmat_root(root)
    startup = ensure_api_startup_file(_gmat_root)
    bin_dir = str(_gmat_root / "bin")
    if bin_dir not in sys.path:
        sys.path.insert(1, bin_dir)
    import gmatpy as gmat  # type: ignore

    gmat.Setup(str(startup))
    # The API does not open the log named in the startup file by itself; give it one so
    # script parse/run errors can be reported (see scenario._log_tail).
    try:
        gmat.UseLogFile(str(log_file_path(_gmat_root)))
    except Exception:
        pass
    if log:
        gmat.EchoLogFile()
    _gmat_module = gmat
    return gmat


def log_file_path(root: Optional[Path] = None) -> Path:
    root = root or gmat_root()
    out = root / "output"
    out.mkdir(exist_ok=True)
    return out / "GmatLog.txt"


def gmat():
    """Return the loaded ``gmatpy`` module, loading it on first use."""
    return load_gmat()
