# Operator-supplied components for the air-gap kit

What a site operator downloads themselves, from the upstream source and pinned by hash, so that
the AltaVista kit never redistributes GPL- or LGPL-licensed code. This continues ADR-003's rule
(the open core carries no copyleft; copyleft or source-available services run unmodified, outside
it, questions 39 and 40) one step further: the kit does not *ship* those components either. It
names each one, pins it, and verifies what the operator brings.

Audited 2026-10-17 at `develop` `999d86e`. This is an engineering inventory, not legal advice;
the licence reviews themselves are the user's (questions 6 and 247).

## How it works

1. On a connected machine, the operator fetches each component below from its upstream, by the
   pin given here (an image digest or a content hash, never a floating tag).
2. The operator carries those files across the air gap beside the kit.
3. The kit's installer verifies each one against its pin before using it, and refuses a mismatch.
   The kit itself carries only AltaVista's own code, its permissively licensed dependencies, and
   the pins.

**Today the kit does not yet work this way** for the two container images and the GMAT pack:
`--with-images` saves whole images (base layers included) and `--copy-pack gmat` copies GMAT's
bundled libraries. The changes that close this are listed at the end ("To make the kit match").

## Components the operator downloads

| # | Component | Why it is operator-supplied | Licences that trigger it | Pin |
|---|---|---|---|---|
| 1 | Ubuntu 22.04 base image (under the cFS image) | OS userland in the image's base layers | GPL-2.0/3.0 (e.g. `grep`, `gzip`, `util-linux`, `procps`, `dpkg`, `apt`), LGPL-2.1 (`libc6`, `libgnutls30`, `libgpg-error0`) | `ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc` |
| 2 | Debian 12 bookworm-slim base image (under the edge-plugin image) | OS userland in the image's base layers | GPL (e.g. `bash`, `coreutils`, `tar`, `sed`, `findutils`, `login`), LGPL (`libc6`, `libgnutls30`, `libgmp10`) | `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171` |
| 3 | GMAT R2026a | Bundles OpenSceneGraph (`libosg*`, `libOpenThreads`, `osgdb_*` plugins) and wxWidgets (`libwx_*`) | OSGPL (LGPL-2.1 with extra permissions), wxWindows Library Licence (LGPL-2.0+ with a binary exception); GMAT itself is Apache-2.0 | content hash of the install tree, below |
| 4 | ClickHouse server (only where the MoE store is deployed) | Static LGPL libraries in the server binary | LGPL (libgsasl, libssh, mariadb-connector-c, numactl, xz, lemmagen-c), GPL-2.0-or-BSD (ittapi), MPL-2.0; ClickHouse itself is Apache-2.0 | `clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4` (26.8.29.10 LTS; arm64 manifest `sha256:b4e4a09d5c05a17640315aeb72aeab3fe03c96a937f414526a7c1ebda3201cb0`). Held out of the kit pending the user's review (question 247) |
| 5 | MinIO (only where the heavy track's object store is deployed) | Network service | AGPL-3.0, run unmodified and reached over S3 HTTP (question 39) | `quay.io/minio/minio@sha256:a1ea29fa28355559ef137d71fc570e508a214ec84ff8083e39bc5428980b015e` (`RELEASE.2025-04-22T22-12-26Z`) |
| 6 | PostgreSQL + PostGIS (only where the catalog is deployed) | Network service | PostGIS is GPL-2.0-or-later (PostgreSQL itself is the permissive PostgreSQL licence), run unmodified over the Postgres wire protocol | `imresamu/postgis@sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd` (`17-3.5-alpine`, PostGIS 3.5.4) |

Items 5 and 6 are not in the kit today and never were. They are listed so a site that deploys the
heavy track knows they come from upstream, by these digests (`services/store/IMAGE_DIGEST.md`,
`services/catalog/IMAGE_DIGEST.md`).

### Item 3: the GMAT pin

The kit's own pack descriptor (`scripts/kit/manifest.py`, `pack_descriptor("gmat", …)`) over this
host's install:

- `content_hash` `sha256:47095a239f72e618685c05c0d5fb3f871587a29b33fd27145f3dd443cd6d3f91`
- 2,072 files, 768,931,073 bytes

That install is the **macOS** build of GMAT R2026a. A Linux site's GMAT is a different tree with
a different hash, so the pin for a target platform is recorded when a kit is built for it.
The operator downloads GMAT R2026a for the target platform from NASA's GMAT release on
SourceForge, unpacks it, and the installer recomputes the descriptor and compares.

## What the kit carries itself (not copyleft)

- **AltaVista's Rust services and binaries.** `cargo deny`'s licence allow-list
  (`deny.toml`) admits only permissive licences plus MPL-2.0. The six Rust SBOMs in
  `docs/compliance/sbom/` list no GPL, LGPL or AGPL component, and no MPL-2.0 one today.
- **`cargo vendor` crate sources (`--with-vendor`).** The same allow-list governs them.
- **The two cross-built Linux binaries (`av-command`, `av-ingest-server`).** Linked dynamically,
  not statically, against `libc.so.6`, `libm.so.6` and `libgcc_s.so.1` (LGPL-2.1, and GPL-3.0
  with the GCC Runtime Library Exception) and OpenSSL 3 (`libssl.so.3`, `libcrypto.so.3`,
  Apache-2.0), all provided by the target host's operating system, never by the kit (measured
  with `ldd` in the edge-plugin image).
- **AltaVista's layers in the two images.** Over its base, the cFS image installs no packages
  (101 in the image, 101 in `ubuntu:22.04`); its added files are the cFS build (Apache-2.0) and
  the lockstep shim (AltaVista's own). The edge-plugin image adds one package, `libssl3`
  (OpenSSL 3, Apache-2.0), and the plugin binary.
- **cFS sources and mirrors (`cfs`, `mirrors` packs).** NASA cFS, cFE, OSAL and PSP,
  Apache-2.0.
- **CSPICE headers (`cspice` pack, 37 files).** NAIF's terms, not copyleft.
- **Time data (`data-time` pack).** AltaVista's own leap-second table.
- **Python wheels (`--with-wheels`).** The viewer's SBOM lists one weak-copyleft package,
  `certifi` (MPL-2.0: file-level, satisfied by shipping it unmodified with a pointer to its
  source). The Linux `numpy` wheel bundles the GCC Fortran runtime (`libgfortran`,
  GPL-3.0-or-later with the GCC Runtime Library Exception, which places no copyleft condition
  on the combined work). On x86_64 the wheel also bundles `libquadmath` (LGPL-2.1+); on the
  kit's aarch64 target it is not expected to. Neither was inspected in a downloaded wheel for
  this audit: the wheel step must list every bundled `.libs` entry (`auditwheel show`) and its
  licence in the kit manifest, and move an LGPL one to this document's operator list.

## Reproducing the audit

- Per-package licences inside an image, from each package's `/usr/share/doc/<pkg>/copyright`:

      docker run --rm --network none --entrypoint sh <image@digest> -c \
        'dpkg-query -W -f "\${Package}\n" | while read p; do
           printf "%s\t%s\n" "$p" "$(grep -h "^License:" /usr/share/doc/$p/copyright 2>/dev/null | sort -u | tr "\n" ";")"
         done'

  This counts a lower bound. `libc6` (LGPL-2.1) and other packages whose copyright file has
  no machine-readable `License:` field print empty and must be read by hand.
- Packages an image adds over its base: run the same command against the base digest and
  compare the package lists.
- Copyleft entries in the committed SBOMs: search `docs/compliance/sbom/*.cdx.json` component
  licences for `GPL`, `LGPL`, `AGPL`, `MPL`, `EPL`, `CDDL`, `EUPL`, `SSPL` or `BUSL`.

## Known gaps in the evidence

- The image SBOMs (`cfs-image.cdx.json`, `edge-plugin-image.cdx.json`) list AltaVista's own
  binaries and name the base image, but not the packages inside it, so they do not show the
  copyleft content this document found. The per-package lists above came from the images
  directly.
- The wheel inspection described above is owed.

## To make the kit match (proposed actions)

1. **Ship application layers, not whole images.** Replace `--with-images`' `docker save` of
   full images with AltaVista's own artefacts plus a runtime Dockerfile `FROM` the pinned base
   digest. The installer builds the image offline on the operator-supplied base and compares
   the result against the recorded runtime-content hash (question 185), since the image ID may
   differ.
2. **GMAT as an operator download.** `--copy-pack gmat` becomes descriptor-only. The installer
   recomputes the descriptor over the operator's GMAT and compares it with the pin recorded for
   that platform.
3. **The installer verifies every item above** by digest or hash before use, refusing a
   mismatch, and prints which operator-supplied components it found.
4. **Extend the image SBOMs** to list the base image's packages and licences, so the kit's own
   evidence shows what the operator is expected to supply.
5. **List bundled native libraries in the wheel step** (`auditwheel show`), as above.
6. **Carry this document in the kit**, beside `KIT_MANIFEST`.
