"""setup.py -- round 3 (question 217(b), the lead's ruling on the viewer packaging gap this
task's own defect report named): the ONE piece of build configuration `pyproject.toml` alone
cannot express -- teaching the built wheel to carry `web/` and `profiles/` (siblings of
`altavista/` at the repo root -- see `pyproject.toml`'s own comment on
`[tool.setuptools.packages.find]`) as `altavista/web/` and `altavista/profiles/` inside the wheel,
WITHOUT moving either source directory. Moving them was explicitly ruled out: dozens of files,
tests, `scripts/kit`, and docs all reference `web/js/...`/`profiles/*.yaml` at the repo root
today, and none of that changes here.

# Why a `build_py` subclass, and why only this one hook

`build_py` is the setuptools command that lays a package's real content out in the BUILD tree
(`build/lib/...`) before it is ever zipped into a wheel -- every other build input this project
needs (`[project]`, `[build-system]`, `[tool.setuptools.packages.find]`,
`[tool.setuptools.package-data]`) already lives in `pyproject.toml`, unchanged, and stays there.
`setup.py`'s only job is the one thing `pyproject.toml`'s declarative tables cannot do: run actual
code during the build. This subclass runs the stock `build_py.run()` FIRST -- every real Python
package (`altavista*`) is laid out exactly as it always was -- and only THEN copies `web/` and
`profiles/` into the build tree as `altavista/web/`/`altavista/profiles/`, using the ordinary
`distutils`/`setuptools` `copy_tree` helper (`self.copy_tree`, inherited from the base command).
Additive by construction: nothing here changes what any existing package/module looks like once
built, in an in-repo run, or under `pip install -e .` (editable installs never invoke `build_py`
against a real build tree the way a real wheel build does) -- it only ADDS two directories nothing
previously packaged at all.

# What happens to the two symlinks inside `web/`

`web/node_modules/three/three.module.js` and `web/node_modules/three/addons` are relative
symlinks re-exporting `web/vendor/three/...` (both confirmed to stay inside `web/`, never
escaping it). `copy_tree`'s default `preserve_symlinks=0` DEREFERENCES a symlink it walks --
copying the real bytes/directory it points at, not a symlink of its own -- which is exactly right
here: a wheel is a zip and cannot carry a symlink at all, and the browser fetches
`node_modules/three/three.module.js` (and everything under `node_modules/three/addons/`) by URL,
so a real file/directory at that path serves it identically to the symlink it replaces. See
`tests/test_kit_zero_egress_install.py`'s own real `GET .../three.module.js` for the proof this
is actually true, not merely assumed.

# Why `package-data`, not `include_package_data`/`MANIFEST.in`

`pyproject.toml`'s `[tool.setuptools.package-data]` table already names `web/**/*`/`profiles/**/*`
under the `altavista` package -- that is what tells the WHEEL step to keep these two directories'
files once `build_py` has put them in the build tree; this file does not repeat that declaration,
it only performs the copy `package-data` alone cannot (that table can only select existing
source-tree files matching a pattern relative to a package's own directory -- `web/`/`profiles/`
do not physically live there in this repository's source tree at all, only after this copy).
"""
from __future__ import annotations

from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py as _build_py

REPO_ROOT = Path(__file__).resolve().parent

#: Repo-root directories that are not their own `packages.find` entry (see this file's own top
#: doc) but must still land inside the built `altavista` package.
_EXTRA_PACKAGE_DIRS = ("web", "profiles")


class build_py(_build_py):
    def run(self) -> None:
        super().run()
        for name in _EXTRA_PACKAGE_DIRS:
            src = REPO_ROOT / name
            if not src.is_dir():
                # A source checkout that genuinely lacks web/ or profiles/ (should not happen in
                # this repository) builds a wheel without that piece rather than failing the
                # whole build -- the same "never guess, never fabricate" posture the rest of this
                # task's code follows, just applied to a missing SOURCE directory instead of a
                # missing profile file.
                continue
            dest = Path(self.build_lib) / "altavista" / name
            self.copy_tree(str(src), str(dest))


setup(cmdclass={"build_py": build_py})
