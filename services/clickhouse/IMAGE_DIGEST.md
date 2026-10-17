# services/clickhouse/IMAGE_DIGEST.md

The sweep study store's ClickHouse server image (question 245: a ClickHouse implementation of the
study store; questions 154, 212, 218, 224 for the pinning rules). Pulled ONCE at setup, by digest,
on 2026-10-17 (heavy-ct round 1a). Question 154: the pull is the one-time setup step; no test ever
pulls. Question 212(a): every image a test depends on records its digest here, and the test
compares the RUNNING image to this record before it trusts it.

**The air-gap kit does NOT carry this image** (question 247): the user holds it out of the kit
pending a licence review of the statically linked LGPL libraries in its binary (see "Licence",
below). The kit's image components are `edge-plugin-image` and `cfs-image` only. This record, the
package inventory (`IMAGE_PACKAGES.txt`) and the licence section are kept as the input for that
review, and for the pin the dev and test paths use. Nothing in `scripts/kit/` reads this file;
`tests/test_clickhouse_image_record.py` fails if the kit starts to, so a re-inclusion is a
deliberate change.

## Image

- Registry reference (pull by digest, never by the tag alone):
```
clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4
```
- Tag this digest carried when it was pulled: `clickhouse/clickhouse-server:26.8.29.10`
  (Docker Hub also served it as `26.8.29` and `26.8`; the `26.8` line is rebuilt about daily)
- `docker image inspect --format '{{.Id}}'` on this host:
```
sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4
```
  On this host's containerd image store the id IS the multi-arch index digest (measured, the same
  as `services/catalog/IMAGE_DIGEST.md`). A running container reports the same value from
  `docker inspect --format '{{.Image}}'` (measured: equal to the id above), which is what a test
  compares.
- Multi-arch index digest: `sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4`
  (OCI index: `linux/amd64`, `linux/arm64`, and one buildx attestation manifest for each)
- arm64 manifest digest: `sha256:b4e4a09d5c05a17640315aeb72aeab3fe03c96a937f414526a7c1ebda3201cb0`
  (the amd64 one is `sha256:e04298b477ba57307348cc64bc085f21b213ddca1e274cfb73695c8a2a0b546b`)
- Architecture: `arm64` (this host is arm64 Colima; the image was built 2026-10-16T16:30:15Z)
- ClickHouse version: `26.8.29.10`, the **latest LTS line**: upstream's release for it is tagged
  `v26.8.29.10-lts` (GitHub, 2026-10-16). Older LTS lines still publishing: `26.3` and `25.8`. The `26.9` and
  `26.7` lines are `-stable`, not LTS.
- Variant: the default (Ubuntu 22.04) image, not `-alpine` or `-distroless`: it has a shell, `wget`
  and a dpkg database, so the entrypoint's init scripts, `docker exec clickhouse-client` and the
  package inventory below all work. Its `Config.Labels` are only `com.clickhouse.build.version=
  26.8.29.10` and `org.opencontainers.image.version=22.04` (the OS, not ClickHouse).
- Size: 262,736,088 bytes of compressed arm64 layers (Docker Hub); 262,754,259 bytes
  (`docker image inspect .Size`); about 1.13 GB unpacked (`docker image ls`); a `docker save`
  tarball is 266,032,128 bytes.

### Kit tag (what a future kit inclusion would need; not wired today)

The kit does not carry this image (question 247, above). Measured in round 1a, while the kit did
carry it, and kept here for whoever re-includes it after the licence review:

`scripts/kit/build_kit.py --with-images` saves, and the installer's loaders load, images by
`repository:tag`, as for the other two kit images. A host that pulled by digest has no tag, so a
kit build would need one local alias, added once after the pull:

```
docker tag clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4 clickhouse/clickhouse-server:26.8.29.10
```

The alias would be trusted for nothing: the save step has to compare the alias's id to the record
above before it runs `docker save`, and refuse on a mismatch. Tests run the digest reference, never
the alias. (Measured: a tarball saved from the alias carries `RepoTags` = the alias; one saved from
the bare digest reference carries none, which `scripts/kit/docker_image_tags.py` cannot find after a
load.) The tarball measured then was 266,032,128 bytes, sha256
`855c07bd1f42efcdadd0f2eb42485d7790b26da17d98540195e1f60c6ad568f6`, identical across independent
`docker save`s (measured four times, including the opt-in two-kit reproducibility test).

**Do not `docker rmi` the alias.** Measured on this host: on a host that holds the image, `docker
rmi clickhouse/clickhouse-server:26.8.29.10` prints `Untagged` and `Deleted: sha256:6ded...` and
removes the image itself; the digest reference is not a second name that survives it. The
zero-egress install test only removes a binding its own `docker load` created, so on a host that
had the alias before the load it changes nothing, and on an air-gapped host that has never seen the
image it loads, verifies by id, and removes it (both measured through `ImageTagGuard`). A host that
pulled by digest and skipped the `docker tag` would instead have the guard delete the pulled image
after the load, so the `docker tag` line in Setup stays: it also protects the local copy. `docker
load -i` of the saved tarball restored the image and the alias with no network (measured: same id,
the digest reference resolves again).

A future inclusion would need: a `clickhouse-image` entry in `scripts/kit/build_kit.py`'s
`IMAGE_DIGEST_SOURCES` and `scripts/kit/sbom.py`'s `IMAGE_COMPONENTS` (with its epoch paths, this
file and `IMAGE_PACKAGES.txt`), an SBOM built from the pin and the inventory, and the user's
licence ruling. Commit `cac4699` is the removed wiring.

## Measured inside the running container (2026-10-17)

- `SELECT version()` -> `26.8.29.10`. Native port, HTTP and MySQL/PostgreSQL wire ports listen on
  `0.0.0.0` inside the container: `8123` HTTP, `9000` native, `9009` interserver HTTP, `9004`
  MySQL, `9005` PostgreSQL (the image `EXPOSE`s 8123, 9000, 9009).
- `GET /ping` on 8123 answers `200 Ok.` without authentication, in every credential scenario
  below. Ready 1.0 s after `docker run` with no `CLICKHOUSE_DB`, 4.0 to 4.1 s with it (the
  entrypoint runs a temporary loopback-only server to create the database, then restarts it; the
  published port refuses until the final server is up).
- **The `default` user, by environment (the entrypoint script, read and measured):**

  | `docker run` environment | Result, measured |
  |---|---|
  | none | the entrypoint logs "neither CLICKHOUSE_USER nor CLICKHOUSE_PASSWORD is set, disabling network access for user 'default'": `default` accepts only `127.0.0.1`/`::1`. From the host through the published port every query is `401` (`Code: 194 ... Authentication failed`). Inside the container `clickhouse-client` works. |
  | `CLICKHOUSE_PASSWORD=pw` only | `default` is recreated with that password, reachable from any address. HTTP basic auth `default:pw` -> `200 26.8.29.10`; no auth or a wrong password -> `401`. |
  | `CLICKHOUSE_USER=u` + `CLICKHOUSE_PASSWORD=pw` | `default` is removed; `u` exists, any address. `u:pw` -> `200`; wrong password -> `403 Code: 516 AUTHENTICATION_FAILED`; no auth -> `401` (user `default` no longer exists). |
  | `CLICKHOUSE_USER=u` only | **`u` is created with an EMPTY password reachable from any address** (`u:` -> `200`). Never run it this way: the image does not refuse. |

  A store test therefore sets `CLICKHOUSE_USER`, `CLICKHOUSE_PASSWORD` (and `CLICKHOUSE_DB`, which
  the entrypoint creates; DDL, `INSERT ... FORMAT JSONEachRow` and `SELECT ... FORMAT JSON` over
  HTTP basic auth were run as that user against it and worked). `CLICKHOUSE_DEFAULT_ACCESS_
  MANAGEMENT=1` (SQL `CREATE USER`) was not needed and was not measured.
- **No HTTPS or secure native port by default.** `https_port` (8443) and `tcp_port_secure` (9440)
  are commented out in `/etc/clickhouse-server/config.xml`; `netstat` inside the container lists
  only 8123, 9000, 9004, 9005, 9009; `system.server_settings` reports `interserver_https_port`
  `0`. The `<openSSL><server>` block exists with `certificateFile`/`privateKeyFile` commented out.
  A TLS deployment adds one file, `/etc/clickhouse-server/config.d/tls.xml`, mounted or baked in
  (the `config.d` directory holds only `docker_related_config.xml`, which sets
  `listen_host` to `::` and `0.0.0.0`):

  ```xml
  <clickhouse>
    <https_port>8443</https_port>
    <tcp_port_secure>9440</tcp_port_secure>
    <openSSL>
      <server>
        <certificateFile>/etc/clickhouse-server/tls/server.crt</certificateFile>
        <privateKeyFile>/etc/clickhouse-server/tls/server.key</privateKeyFile>
      </server>
    </openSSL>
  </clickhouse>
  ```

  with the certificate and key readable by uid 101 (`clickhouse`). **This TLS file was NOT run**:
  generating a test certificate needs the `openssl` CLI, which this host's shell policy refuses
  and which a worker may not wrap. Everything in this block other than the default-off facts above
  is the shape the image's own commented sample implies; the worker who adds TLS must measure it.
- **The image declares `VOLUME /var/lib/clickhouse`.** Every `docker run` therefore creates an
  anonymous volume that `docker rm -f` leaves behind (measured: the first four measurement runs
  left four, removed by name at the end of this task). Run with `--tmpfs /var/lib/clickhouse`
  (measured: `Mounts=[]`, no volume created, DDL/INSERT/SELECT work, about 161 MiB resident when
  idle) or remove with `docker rm -fv`. No bind mount, for the same Colima reason
  `services/store/IMAGE_DIGEST.md` records.

## Running it (tests do this themselves, under the host-wide docker lock)

```
docker run -d --label av.test=1 --label av.test.run_id=<id> \
  -e CLICKHOUSE_USER=<user> -e CLICKHOUSE_PASSWORD=<password> -e CLICKHOUSE_DB=<db> \
  -p 127.0.0.1::8123 -p 127.0.0.1::9000 \
  --tmpfs /var/lib/clickhouse \
  clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4
```

Readiness: poll `GET http://127.0.0.1:<port from docker port>/ping` for `200`. Queries: HTTP on the
published 8123 with basic auth (`?query=...` or a POST body). `-p 127.0.0.1::9000` is only for a
native-protocol client; drop it if the store speaks HTTP alone. Compare
`docker inspect --format '{{.Image}}' <container>` to the id above before trusting the run.

## Setup, once per host (question 154: the only network use)

Both lines are required (see "Do not `docker rmi` the alias", above; the alias is for local
copy safety and for a future kit inclusion, since the kit does not carry the image):

```
docker pull clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4
docker tag  clickhouse/clickhouse-server@sha256:6ded2b2c2f2c46f12784bd8000c75f8aa7363372ed64afccaa932456b8cb41e4 clickhouse/clickhouse-server:26.8.29.10
```

Check `colima ssh -- df -h /var/lib/docker` first (the pull is about 0.3 GB compressed). To move to
a newer patch release, repeat the lookup (Docker Hub tags API, then the registry's manifest for the
tag), pull the new index digest, and rewrite every digest and the package inventory here in one
change.

## Package inventory

`services/clickhouse/IMAGE_PACKAGES.txt` lists the 111 dpkg packages (name, version, architecture)
read from the image's `/var/lib/dpkg/status` with no network. It is kept as input for the licence
review (question 247); no SBOM is generated from it today (an earlier kit SBOM, `cac4699`, was
removed with the kit wiring). Regenerate it with `services/clickhouse/extract_image_packages.py`
(its header gives the commands). The upstream index also carries a buildx SPDX SBOM and SLSA
provenance attestation for the arm64 image (attestation manifest
`sha256:e6ce34315d22e655c850fecc7229c822028a111dcd2d6c36c0225175ed22e89c`, SPDX blob about
3.2 MB); it was NOT fetched, because that would be a second network fetch for this image.

## Licence

- Licence (SPDX): `Apache-2.0` (ClickHouse server, client and common packages).
- **Not confirmable from the image itself, and the brief's two in-image checks both came up
  empty.** dpkg lists `/usr/share/doc/clickhouse-server/LICENSE` (and the same for
  `clickhouse-client` and `clickhouse-common-static`), but those directories are empty in the
  image (the image build strips them), so there is no `LICENSE` file to quote. The image has no
  `org.opencontainers.image.licenses` label (its two labels are above). What the record rests on:
  upstream, the ClickHouse repository's `LICENSE` is Apache-2.0 (GitHub's licence API for
  `ClickHouse/ClickHouse`, read 2026-10-17: `spdx_id` `Apache-2.0`, path `LICENSE`, blob
  `f1da7e1a39450a88d02db05bbf213cdb25805a5a`); the packages' dpkg `Homepage` is
  `https://clickhouse.com`; and the `clickhouse` binary embeds Apache-2.0 notices (which the
  libraries linked into it also carry, so that alone is not proof). If an in-image confirmation
  is required, the attestation SBOM above is the next source.
- Base OS layer: **Ubuntu 22.04.5 LTS (Jammy Jellyfish)**, aarch64. It brings its own package
  licences: 111 distro packages, each under its own terms (GPL, LGPL, BSD, MIT and others), stated
  per package in `/usr/share/doc/<package>/copyright` inside the image. The package inventory
  records name, version and architecture only; the per-package licences are not extracted, which a
  review would have to read from the image.
- Statically linked libraries: the `clickhouse` binary's own `system.licenses` table lists 1,443
  bundled libraries (mostly MIT, Apache-2.0 and dual-licensed Rust crates). Rows that are not
  permissive: LGPL for `lemmagen-c`, `libgsasl`, `libssh`, `mariadb-connector-c`, `numactl` and
  `xz`; `GPL-2.0-only OR BSD-3-Clause` for `ittapi` and `ittapi-sys`; `MIT OR Apache-2.0 OR
  LGPL-2.1-or-later` for `r-efi`/`r-efi-alloc`; MPL-2.0 for `cbindgen`, `defer-drop`,
  `fortanix-sgx-abi`, `shuffling-allocator`, `timer` and `webpki-roots`; one each of OpenLDAP and
  PostgreSQL. They are upstream's own distribution choices, listed here because shipping the
  image in an enclave kit would redistribute it. **Question 247: the user holds the image out of the
  kit pending a licence review (notices and any GPL/LGPL source-offer terms) of these statically
  linked libraries. This section and the package inventory are the review's input.**
