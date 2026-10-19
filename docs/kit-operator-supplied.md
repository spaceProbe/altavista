# Operator-supplied components for the air-gap kit

What a site operator brings themselves, from the upstream source and pinned by hash or digest, so
that the AltaVista kit never redistributes GPL- or LGPL-licensed code. This continues ADR-003's rule
(the open core carries no copyleft; copyleft or source-available services run unmodified, outside
it, questions 39 and 40) one step further: the kit does not *ship* those components either. It
names each one, pins it, and verifies what the operator brings.

Audited 2026-10-17 at `develop` `999d86e`; revised 2026-10-17 to describe the kit as `edge` builds it
at `b08e869` (question 250(a), tasks 1a to 7), with the changes of task 6 on top. This is an
engineering inventory, not legal advice; the licence reviews themselves are the user's (questions 6
and 247).

The kit carries this file at its root, as `kit-operator-supplied.md`, byte for byte (listed in
`KIT_MANIFEST.files` with the role `operator-doc` and hashed like every file). The installer refuses
a kit that lacks it and prints its path before the list of components.

## How it works

The kit carries AltaVista's own code, its permissively licensed dependencies, and the pins. Every
component in the table below comes from the operator.

1. **On a connected machine**, the operator fetches each component the site needs, by the pin in
   the table (an image digest or a content hash, never a floating tag):
   - the two base images, saved by digest: `.venv/bin/python scripts/kit/save_base_images.py --out DIR`
     (runs `docker save <name>@sha256:<digest>` for each pin and writes `DIR/SHA256SUMS`);
   - the operator-supplied wheel (item 7): `.venv/bin/python scripts/kit/save_operator_wheels.py --out DIR`
     (`pip download` of exactly the pinned file; refuses unless its name and sha256 match;
     writes `DIR/SHA256SUMS`);
   - GMAT R2026a (item 3), downloaded and unpacked;
   - a service image (items 5 and 6), only if the site deploys that service, by its digest.

   The two helpers live in the repository's `scripts/kit/`; the kit does not carry them. The kit
   carries the pins (`installer/operator_supplied.json`) and this document, which is what an
   operator holding only the kit needs to fetch the same files by hand.
2. **The operator carries those files across the air gap** beside the kit.
3. **The files install**, run inside the enclave, verifies the trees and wheels it can reach:
   `<kit>/installer/install.sh <kit> <target> --gmat-root PATH --gmat-platform KEY --operator-wheels DIR`.
   GMAT is measured and compared with the pin for `KEY` (`macos-arm64`, say), never copied; the
   operator's wheel must have the pinned sha256, and pip installs a copy of exactly that file by
   path. It never holds the Docker socket, `DOCKER_HOST` or any other path to a daemon, because a
   container that controls the daemon could start a networked container and defeat the isolation
   the zero-egress proof rests on (the lead's ruling, question 250(a)).
4. **The image step**, run by the operator on the Docker host, builds the two images:
   `<kit>/installer/install.sh --build-images <kit>`. It follows the order below.
   1. The kit is verified against `KIT_MANIFEST`, and each image's recorded base against the pins.
   2. Each base is looked up by digest on the host (`docker image inspect`: the image `Id`, or a
      `RepoDigests` entry, equals the pin). The operator loads the saved tarball first
      (`docker load -i`, after checking `SHA256SUMS`). Nothing is pulled.
   3. The image is built from the kit's runtime Dockerfile, `FROM <base>@<digest>`, with
      `docker build --network none --pull=false`: the application layer on the operator's base.
   4. Before anything is started, the built image is created (never started) and its runtime
      files are read out. Their runtime-content hash (question 185) must equal the recorded one,
      every file's mode must equal the recorded mode, and `.Config` must equal the recorded
      `.Config`. The image id is reported, not compared: it may differ from the recorded one.
   5. `IMAGE_BUILD_RECORD` is written.

   `<kit>/installer/install.sh --probe-offline-build <kit>` shows that the build itself is offline
   (throwaway images whose `RUN` tries the network must fail).
5. **Both steps print the same list**, one line per component of the pins file, always all
   seven, in item order. For every component the kit needs, one step says `verified ...` and the
   other says `verified by the ... , not here`; `NOT VERIFIED [code]` is a refusal and that step
   stops there. `<kit>/installer/install.sh --check-operator-supplied <kit> [--gmat-root ...]`
   runs the first step's verification and prints the list without installing.
6. **The pins file** is `scripts/kit/operator_supplied.json` in the repository and
   `installer/operator_supplied.json` in a kit. It pins items 1 to 7 with their kind, licences and
   verification. Item 8 is a version floor, checked by `install.sh` itself.
7. **Which components a kit needs** follows from what it carries: a base image for each image
   application layer it carries, GMAT when it records the `gmat` pack, the wheel when the wheel step
   found a GPL-family library in it, and a service image only when the kit was built with
   `build_kit.py --declare-operator-supplied NAME` (see "Items 4 to 6").

## Components the operator brings

| # | Component | Why it is operator-supplied | Licences that trigger it | Pin |
|---|---|---|---|---|
| 1 | Ubuntu 22.04 base image (under the cFS image) | OS userland in the image's base layers | GPL-2.0/3.0 (e.g. `grep`, `gzip`, `util-linux`, `procps`, `dpkg`, `apt`), LGPL-2.1 (`libc6`, `libgnutls30`, `libgpg-error0`) | `ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc` |
| 2 | Debian 12 bookworm-slim base image (under the edge-plugin image) | OS userland in the image's base layers | GPL (e.g. `bash`, `coreutils`, `tar`, `sed`, `findutils`, `login`), LGPL (`libc6`, `libgnutls30`, `libgmp10`) | `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171` |
| 3 | GMAT R2026a | Bundles OpenSceneGraph (`libosg*`, `libOpenThreads`, `osgdb_*` plugins) and wxWidgets (`libwx_*`) | OSGPL (LGPL-2.1 with extra permissions), wxWindows Library Licence (LGPL-2.0+ with a binary exception); GMAT itself is Apache-2.0 | content hash of the unpacked tree under a scope, per platform, below |
| 4 | ClickHouse server (only where the MoE store is deployed) | Static LGPL libraries in the server binary | LGPL (libgsasl, libssh, mariadb-connector-c, numactl, xz, lemmagen-c), GPL-2.0-or-BSD (ittapi), MPL-2.0; ClickHouse itself is Apache-2.0 | `clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4` (26.8.29.10 LTS; arm64 manifest `sha256:b4e4a09d5c05a17640315aeb72aeab3fe03c96a937f414526a7c1ebda3201cb0`). Held out of the kit pending the user's review (question 247) |
| 5 | MinIO (only where the heavy track's object store is deployed) | Network service | AGPL-3.0, run unmodified and reached over S3 HTTP (question 39) | `quay.io/minio/minio@sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e` (`RELEASE.2025-04-22T22-12-26Z`) |
| 6 | PostgreSQL + PostGIS (only where the catalog is deployed) | Network service | PostGIS is GPL-2.0-or-later (PostgreSQL itself is the permissive PostgreSQL licence), run unmodified over the Postgres wire protocol | `imresamu/postgis@sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd` (`17-3.5-alpine`, PostGIS 3.5.4) |
| 7 | numpy 2.5.3 wheel for linux/aarch64/cp313 (only for a kit built with `--with-wheels`) | The wheel bundles `libgfortran`, GPL code | `libgfortran`: GPL-3.0-or-later WITH GCC-exception-3.1. The wheel's other bundled library, OpenBLAS, is BSD-3-Clause AND BSD-3-Clause-Open-MPI; numpy itself is BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0 | `numpy-2.5.3-cp313-cp313-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl`, `sha256:c76d5dde9f445058f83d0c02af00557a4db91de9a9a57c0df87d1535001d654b`, from PyPI |
| 8 | Python 3.11 or newer, on the host that runs the installer (the enclave host for the files install, and the Docker host for the image step) | The kit carries no interpreter, and `install.sh` runs `install.py` with the operator's | None: the PSF licence is not copyleft. Listed because the kit does not carry it and the install cannot start without it | A version floor, not a hash: Python >= 3.11 (the standard library's `tomllib`, which the bundled modules use) |

### Items 1 and 2: the base images

The kit carries each image's application layer and a Dockerfile that starts `FROM` the pinned base
digest (`images/<component>/Dockerfile`, committed as `services/cfs/Dockerfile.kit` and
`services/edge-plugin/Dockerfile.kit`). The base's 101 (Ubuntu) and 88 (Debian) packages are listed
in the image SBOMs, each marked operator-supplied, from the committed
`services/cfs/BASE_IMAGE_INVENTORY.json` and `services/edge-plugin/BASE_IMAGE_INVENTORY.json`
(generated by `scripts/kit/base_inventory.py`).

### Item 3: the GMAT pin

The pin is per platform, in the `gmat-r2026a` component of the pins file, and holds the content hash,
file count, total bytes, symlink count and a hash of the symlink listing (`manifest.measure_tree`,
the one function the builder and the installer both call):

| Platform key | Pin |
|---|---|
| `macos-arm64` | `content_hash` `sha256:d21cddbbffbd3c44982bd98a7fb3a221b79ae6e26f2696eff4046f745d1eca39`; 2,060 files, 764,662,632 bytes; 183 symlinks, listing hash `sha256:f788ebf2fa73ef4d22e8ca2f4c1d51bb92a6a30dda34d0366ef19c3a719b5420` |
| `linux-x86_64` | `content_hash` `sha256:e15babfc251e14fdd5d306970d9698d634e41c6ed5957118c31af71d28f466f3`; 2,103 files, 689,762,981 bytes; 114 symlinks, listing hash `sha256:fe5ab4958e93ae79ade6b18dde99572e63f47c880bc51590efd66e0452401948`. Measured from `gmat-ubuntu-x64-R2026a.tar.gz` (below) |
| `linux-aarch64` | **No pin yet, and none can be made yet:** NASA publishes no Linux aarch64 build of R2026a (below). The installer refuses (`gmat-no-pin-for-platform`) for a kit that needs GMAT on this key, naming the platforms that have a pin |

The key is `<os>-<uname -m>`. The operator gives it with `--gmat-platform`; the installer does not
guess it, because it also runs in a Linux container over a macOS tree.

**What the pin leaves out, and why.** The scope excludes `output/`, `bin/api_startup_file.txt`, and
every `__pycache__/` directory. This host's whole-tree measurement is not stable, because GMAT
rewrites the tree it is installed in: it rewrites `output/GmatLog.txt` on every run, its Python API
leaves bytecode under `bin/gmatpy/**/__pycache__/`, and `api/BuildApiStartupFile.py` generates
`bin/api_startup_file.txt` holding the install's absolute path, so the same release unpacked
elsewhere hashes differently. This document's first revision recorded the whole-tree descriptor,
`sha256:47095a23…` over 2,072 files and 768,931,073 bytes; that value was taken before a run changed
`output/GmatLog.txt` (the whole tree now reads 768,932,620 bytes), and it was never a property of
the release. The twelve files the scope leaves out are exactly the files of this host's install that are
newer than the release (`find . -type f -newermt 2026-03-27`). The pin is therefore a statement about the release's files, not the operator's
working tree.

**What the pin does not cover**: file modes, ownership, timestamps, extended attributes and empty
directories; the files in the scope's exclusions (including the generated startup file, which lists
the plugins GMAT loads and is the operator's to generate); and any change after the install-time
check. A match is recorded in `INSTALL_RECORD`; nothing is copied.

The operator downloads GMAT R2026a for the target platform from NASA's GMAT release on SourceForge
and unpacks it.

**The Linux release (question 254, task 12).** NASA's R2026a folder on SourceForge
(`https://sourceforge.net/projects/gmat/files/GMAT/GMAT-R2026a/`, read 2026-10-18) lists two Linux
archives, both x86_64, and none for aarch64:

| Archive | Bytes | sha256 (measured) | SourceForge's SHA-1 / MD5 (both matched) | Pin |
|---|---|---|---|---|
| `gmat-ubuntu-x64-R2026a.tar.gz` (SourceForge's default Linux download) | 401,664,992 | `fe124b4a606b2e3b704a6fbb1c37b87598d5df0d18cb661c304b5f60074a7754` | `15bb6057a25853ea68f2c0c781e030d4beae3348` / `ec496a0319657fa2c079d7e996b9b1ed` | `linux-x86_64` |
| `gmat-rhel7-x64-R2026a.tar.gz` | 438,017,278 | `3262cf77c3932855180f346705605a89306ffd6cff15d70ad8bdafdfc0b614cd` | `e6f4ca0ffefa3cb5a443cd24417b8778e709c74f` / `f4e5e3668f447833d29bac40e33d99c9` | none (measured only, below) |

Each was fetched once (question 154) with `curl -fL --proto '=https' --proto-redir '=https'` from
`<archive URL>/download` under `https://sourceforge.net/projects/gmat/files/GMAT/GMAT-R2026a/`, which
redirects to a mirror (`cytranet-dal.dl.sourceforge.net` for the Ubuntu archive,
`master.dl.sourceforge.net` for the RHEL 7 one) under
`/project/gmat/GMAT/GMAT-R2026a/`; TLS was verified and the request carried no header of ours. The
pins file records both under `archives` (URL, final mirror URL without its per-request query, size,
sha256, SourceForge's checksums). The archive unpacks to `GMAT/R2026a/`, and that directory is the tree
the pin measures (`--gmat-root`). The archive's own `output/` is an empty directory, so the scoped and
the whole-tree measurements are equal for a freshly unpacked release. The archive was extracted
with `tar -xzf` on macOS; its 2,103 files and 114 symlinks (Ubuntu) all arrived, and no two
names differ only by case.

The RHEL 7 archive is a different tree (2,043 files, 774,131,578 bytes, 100 symlinks; content hash
`sha256:3db3afed6d28291932f8ed4d06f20b2df09298b3e02f8a50e156c22b82b3975c`, listing hash
`sha256:041a4fffcbd08b17664951fa02eff53627cff2e256cccafc6299348641bd0a8e`). A platform key holds one pin,
so `linux-x86_64` pins the Ubuntu archive and the RHEL 7 measurement is recorded, not pinned; an
operator on the RHEL 7 build is refused with `gmat-descriptor-differs`, and a second key (for example
`linux-x86_64-rhel7`) would pin it if a site needs it. A pin for Linux was measured, not run: Colima's VM is
aarch64, so the x86_64 binaries were never started; the installer's check reads bytes only.

### Items 4 to 6: service images and ClickHouse

Items 5 and 6 are not in the kit and never were. A site that deploys the heavy track builds its kit
with `build_kit.py --declare-operator-supplied minio --declare-operator-supplied postgis`; the
names go into `KIT_MANIFEST.operator_supplied_declared` (empty by default). The image step then
verifies each declared image by digest, with the same lookup as for a base (`docker image inspect`:
`Id` or `RepoDigests` equals the pin), and refuses before any build if one is absent or different
(`operator-image-missing`, `operator-image-digest-differs`, naming reference and digest). It does not
pull or load it. An undeclared service is listed as `outside the kit, not verified`. The pins match
`services/store/IMAGE_DIGEST.md` and `services/catalog/IMAGE_DIGEST.md`.

ClickHouse (item 4) cannot be declared: the builder refuses the name and the installer refuses a
manifest that declares it (question 247, held pending the user's licence review). Its digest and
arm64 manifest digest are pinned here and in the pins file so they are recorded; the list reports it
as outside the kit.

### Item 7: wheels that bundle GPL-family libraries

The wheel step (`--with-wheels`) opens every wheel it fetches and inventories the native libraries it
bundles, with licences read from the committed `scripts/kit/bundled_libs.json` and confirmed by
the wheel's own licence text (`KIT_MANIFEST.wheels.fetched[*].bundled_libraries`).

**Ruling (lead, native round of question 250).** A bundled library with any GPL-family licence term
(GPL, LGPL or AGPL, with or without the GCC Runtime Library Exception) moves its wheel to the
operator list. The exception frees numpy as a combined work, but the `libgfortran` file is itself
GPL-3.0 code, and the instruction is that the kit redistribute no GPL or LGPL code. An earlier
revision of this document kept numpy in the kit because the exception "places no copyleft condition
on the combined work"; that reasoning is withdrawn.

Measured on the numpy 2.5.3 linux/aarch64 wheel: it bundles exactly two libraries,
`numpy.libs/libgfortran-e1b7dfc8-d8198c01.so.5.0.0` (GPL-3.0-or-later WITH GCC-exception-3.1,
stanza in the wheel's `numpy-2.5.3.dist-info/licenses/LICENSE.txt`, lines 131 to 155) and
`numpy.libs/libscipy_openblas64_-f552eb69.so` (BSD-3-Clause AND BSD-3-Clause-Open-MPI, lines 40 to
128). It bundles no `libquadmath` on aarch64. No other wheel in the closure bundles a library.
numpy's licence text also names a `libquadmath` stanza (LGPL-2.1-or-later), which a wheel for another
platform may bundle; `bundled_libs.json` has the entry, but no x86_64 wheel was fetched or inspected.

So a `--with-wheels` kit does not carry the numpy wheel: `KIT_MANIFEST.wheels.operator_supplied`
records its name and sha256, and the installer needs `--operator-wheels DIR` with the pinned file in
it (`wheel-dir-missing`, `wheel-missing`, `wheel-hash-differs`, ... are typed refusals before the
target directory exists). Any other wheel found to bundle a GPL-family library moves here the same
way; the builder refuses to build unless the pins file has a `python-wheel` component for exactly
that file.

### Item 8: the Python interpreter

`install.sh` is POSIX shell and runs `install.py` with the first of `python3.13`, `python3.12`,
`python3.11`, `python3` and `python` on `PATH` that is Python 3.11 or newer, or with `$PYTHON` as
given, and refuses (exit 1) when it finds none. The version floor comes from the bundled `sbom.py`
reading TOML with `tomllib`; measured on this host, where a bare `python3` is 3.9.6, an older
interpreter died in a `ModuleNotFoundError` (that is why `install.sh` looks for a new enough one). It
applies on the enclave host for the files install and on the Docker host for the image step, which
also needs a Docker engine and CLI. Neither the interpreter nor the engine is pinned here. The zero-egress
proof runs the files install in a `python:3.13-slim` container.

## What the kit carries itself (not copyleft)

- **AltaVista's Rust services and binaries.** `cargo deny`'s licence allow-list
  (`deny.toml`) admits only permissive licences plus MPL-2.0. The six Rust SBOMs in
  `docs/compliance/sbom/` list no GPL, LGPL, AGPL or MPL component (the committed copies predate
  `d7a212b` and are regenerated by the lead; `cargo deny` passes on the new tree). YAML is parsed by `av-yaml`
  over `serde-saphyr` (question 246, `d7a212b`), which replaced `serde_yaml`; `deny.toml` bans
  `serde_yaml` outside `spoore-tree` and admits `serde-saphyr` only through `av-yaml`.
- **`cargo vendor` crate sources (`--with-vendor`).** The same allow-list governs them. Two of the
  vendored crates `serde-saphyr` brought in, `encoding_rs` and `multiversion_no_op`, carry a
  `LICENSE-APACHE` that is byte-identical to the canonical Apache License 2.0 text which both base
  images ship (`/usr/share/common-licenses/Apache-2.0`, `base-files`). A permissive licence's own text
  is not base-image userland, so the overlap is exempted by exact sha256 (`cfc7749b96f6…`) at any
  kit path, and for no other content (see the "Two licence texts" entry below).
- **The two cross-built Linux binaries (`av-command`, `av-ingest-server`).** Linked dynamically,
  not statically, against `libc.so.6`, `libm.so.6` and `libgcc_s.so.1` (LGPL-2.1, and GPL-3.0
  with the GCC Runtime Library Exception) and OpenSSL 3 (`libssl.so.3`, `libcrypto.so.3`,
  Apache-2.0), all provided by the target host's operating system, never by the kit (measured
  with `ldd` in the edge-plugin image; carried from the first audit, not re-measured since).
- **AltaVista's layers in the two images** (`--with-images`; a runtime Dockerfile, the files at
  their image paths with their modes, and for the edge-plugin image a `.deb`; no `docker save`
  tarball and no base layer).
  - *cFS image.* Over its Ubuntu base the image installs no packages (101 in the base's
    inventory). The 13 files the kit carries are the cFS build under `/cfs/cpu1` (NASA cFS, cFE,
    OSAL and PSP, Apache-2.0), AltaVista's lockstep shim and relay, and the container entrypoint.
    Recorded image `sha256:e51b896d…`, runtime-content hash `sha256:56d7bd00…`
    (`services/cfs/IMAGE_DIGEST.md`; the record names the build commit after question 246's
    lock-file change, the image is unchanged).
  - *Edge-plugin image.* Over its Debian base (88 packages, none of them `libssl3`) the image adds
    one package, `libssl3` 3.0.22-1~deb12u1 (OpenSSL 3, Apache-2.0), and the plugin binary,
    fixtures and default configuration: 11 files, 7 of them from the package. The recorded image id is
    `sha256:34d4e440…` (re-pinned from `72e727a8…` when the image was rebuilt; the runtime files and
    the plugin binary are byte-identical), runtime-content hash `sha256:c860fcd4…`
    (`services/edge-plugin/IMAGE_DIGEST.md`).
  - **Why the `libssl3` `.deb` is carried and not operator-supplied (lead's ruling).** Every file the
    package ships is Apache-2.0: its data member is the three engines, the two shared libraries,
    `legacy.so` and three documentation files. The copyright file's "Artistic or GPL-1+" stanza
    covers `external/perl/Text-Template-1.56/*`, a Perl module in OpenSSL's source tree that its
    build uses; it is a build-time source helper, not in the package, and the `.deb` holds no file
    from it (`services/edge-plugin/runtime-packages.json`). The `.deb` is pinned by sha256
    (`026105b9…`, 1,827,612 bytes) and by its `snapshot.debian.org` URL, fetched once at kit-build
    time over verified TLS, and the builder proves every regular file the image holds from the package
    equals the `.deb`'s byte for byte. The licence review remains the user's.
  - **Two licence texts are byte-identical to base-image files, and exempted by exact hash**
    (`scripts/kit/base_overlap_exceptions.json`, the one definition the inventory proof and the
    zero-egress proof both read). One is the canonical Apache-2.0 text, above. The other is the
    `libssl3` copyright notice, which Ubuntu's own `libssl3` package ships byte for byte; it is accepted
    at three exact kit paths (the notice file beside the `.deb`, its member in the `.deb`, and the
    image-layer entry) and nowhere else. The same bytes at any other path fail, and so does any other
    overlap with a base-image file.
- **cFS sources and mirrors (`cfs`, `mirrors` packs).** NASA cFS, cFE, OSAL and PSP,
  Apache-2.0.
- **CSPICE headers (`cspice` pack, 37 files).** NAIF's terms, not copyleft.
- **Time data (`data-time` pack).** AltaVista's own leap-second table.
- **Python wheels (`--with-wheels`).** The linux/aarch64/cp313 closure is 24 wheels: the viewer's
  runtime dependencies and the `altavista` wheel itself, and not numpy (item 7). The closure includes
  `httpx` and `pyyaml`, which were imported by the viewer at run time but undeclared, so an installed
  viewer failed with `ModuleNotFoundError: httpx` until `3594bf5` declared both in `pyproject.toml`, the
  builder and the installer's required wheels. Every wheel is opened and its bundled native libraries
  inventoried: none of the 24 bundles one (the inventory is empty for each; statically linked code has
  no separate file and is not inventoried). The viewer's SBOM, and `gmat-service`'s, list one weak-copyleft
  package, `certifi` (MPL-2.0: file-level, satisfied by shipping it unmodified with a pointer to its
  source); `scripts/kit/copyleft_accepted.json` is the one place that accepts it.

## Reproducing the audit

- Per-package licences and files inside a base image: the committed inventories
  `services/cfs/BASE_IMAGE_INVENTORY.json` and `services/edge-plugin/BASE_IMAGE_INVENTORY.json`;
  `.venv/bin/python scripts/kit/base_inventory.py --check` regenerates them in memory from the
  digest-pinned base (`docker run --pull never --network none`; it takes the docker lock itself)
  and exits 2 if they are stale. Of the 101 Ubuntu packages, 48 carry a GPL, LGPL or AGPL licence
  string and 46 have no machine-readable `License:` field; of the 88 Debian packages, 67 and 13.
  `libc6` (LGPL-2.1) is among those with no field and is read by hand. By hand, in an image:

      docker run --rm --network none --entrypoint sh <image@digest> -c \
        'dpkg-query -W -f "\${Package}\n" | while read p; do
           printf "%s\t%s\n" "$p" "$(grep -h "^License:" /usr/share/doc/$p/copyright 2>/dev/null | sort -u | tr "\n" ";")"
         done'

- Packages an image adds over its base: the cFS image adds none; the edge-plugin image adds
  `libssl3`, recorded in `services/edge-plugin/runtime-packages.json`.
- Copyleft entries in the SBOMs: search `docs/compliance/sbom/*.cdx.json` component licences for
  `GPL`, `LGPL`, `AGPL`, `MPL`, `EPL`, `CDDL`, `EUPL`, `SSPL` or `BUSL`. The image SBOMs, as
  `scripts/kit/sbom.py` now generates them, list every base package with
  `altavista:sbom:operator-supplied=true`; the committed copies are regenerated by the lead after
  the round merges.
- That the kit carries no file of a base-image package, and that every operator-supplied marker is
  true: `.venv/bin/python scripts/kit/kit_inventory.py KIT [--sbom-dir DIR]` and
  `tests/test_kit_no_copyleft.py` (gated by `AV_KIT_WITH_IMAGES=1` over `out/kit/proof`). Over the
  proof kit it compared 13,060 items with the 5,885 files of both bases, by image path and by
  sha256: 0 failing matches, 5 accepted by name (the three `libssl3` copyright paths and the two
  Apache-2.0 texts), 1 trivial (a one-byte file, compared by path). The SBOM copies in the kit give 2
  copyleft licence strings (`certifi`, accepted); the image SBOMs as now generated give 416, all
  operator-supplied. A GPL-family library inside a carried wheel fails the same test.
- The wheel inventory: `KIT_MANIFEST.wheels.fetched[*].bundled_libraries`, and the
  `wheels.operator_supplied` list.
- Re-measuring the GMAT pins: `AV_KIT_MEASURE_GMAT=1 GMAT_ROOT=<unpacked macOS release> pytest
  tests/test_kit_install.py` (the test `test_the_real_gmat_tree_matches_the_recorded_macos_pin`), and
  `AV_KIT_MEASURE_GMAT_LINUX=1 GMAT_LINUX_ROOT=<GMAT/R2026a of the unpacked Ubuntu archive> pytest
  tests/test_kit_install.py` (`test_the_real_linux_gmat_tree_matches_the_recorded_linux_x86_64_pin`).
- The zero-egress proof, `tests/test_kit_zero_egress_install.py`, with the bases from
  `out/kit/proof-bases/` and numpy from `out/kit/proof-operator-wheels/` supplied separately and a kit
  carrying only application layers: 5 passed.

## Known gaps in the evidence

Closed in this round: the image SBOMs now list the base packages and licences (task 4, `5ad7b68`),
and every wheel's bundled libraries are inventoried (task 5, `03f0504`).

- The licence data for the bases is Debian's short names read from each package's copyright file. It
  is a lower bound: 46 of the 101 Ubuntu packages and 13 of the 88 Debian packages have no
  machine-readable `License:` field and appear in the SBOMs as a named gap.
- The committed `docs/compliance/` SBOMs (the image SBOMs, and `av-viewer` and `gmat-service`
  after the `httpx` and `pyyaml` declarations) are regenerated by the lead after the merge; until then
  they show the earlier component lists.
- GMAT has pins for macOS arm64 and Linux x86_64 (the Ubuntu archive; question 254, task 12). There is
  no Linux aarch64 pin, because NASA publishes no such build, and the RHEL 7 archive is measured and
  recorded, not pinned. The macOS pin is for the `macos-arm64` key; SourceForge's R2026a folder lists a
  macOS x64 `.dmg` only, and this document does not record where the arm64 tree came from. The pins do
  not cover what their scope leaves out, nor anything after the install-time check.
- The wheel pin is for numpy 2.5.3 on linux/aarch64/cp313. A closure for another platform, or another
  numpy version, needs its own `python-wheel` pin before the builder will produce the kit.
- The two helpers that make the operator's files (`save_base_images.py`, `save_operator_wheels.py`) are
  in the repository, not in the kit.
- The kit-inventory proof does not open an archive nested inside a wheel or crate, other than the
  `ar` libraries and the `.deb`; it found none in the proof kit.
- The dynamic-linking statement about the two cross-built binaries is from the first audit.
- The licence reviews, including that of the `libssl3` ruling and ClickHouse (question 247), are the
  user's.

## To make the kit match

1. **Ship application layers, not whole images.** Done: `a65ce85` carries each image's runtime
   Dockerfile, files with modes and the `libssl3` `.deb`; `3d0b429` adds the host-side step that
   builds on the operator's base and compares the runtime-content hash, modes and `.Config`;
   `dc8eb1d` re-records the cFS build commit.
2. **GMAT as an operator download.** Done: `b53ec76` makes `--copy-pack gmat` an error and the
   installer measures the operator's tree against a per-platform pin. Linux x86_64 pinned in task 12
   of question 254; Linux aarch64 has no NASA build.
3. **The installer verifies every item above** by digest or hash before use, refusing a mismatch,
   and prints the list. Done: `bec6611` for items 1 to 6 (and `--declare-operator-supplied`,
   `--check-operator-supplied`), extended to item 7 by `03f0504`. Item 8 is checked by `install.sh`
   itself and not listed.
4. **Extend the image SBOMs** to list the base image's packages and licences. Done in the generator:
   `5ad7b68`. Open: the lead regenerates the committed copies.
5. **List bundled native libraries in the wheel step**, and move a GPL-family wheel here. Done:
   `03f0504`; numpy's linux/aarch64 wheel is item 7.
6. **Carry this document in the kit**, beside `KIT_MANIFEST`. Done in task 6 (this revision's
   commit).
7. **Prove it.** Done: `b08e869` (the kit carries no base-image content, and the markers are
   true), `3594bf5` (the viewer's undeclared dependencies, found by the zero-egress proof) and the
   zero-egress proof passing.
8. **Open: ClickHouse review (question 247); regeneration of the committed SBOMs.**
