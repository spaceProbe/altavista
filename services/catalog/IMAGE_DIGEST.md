# services/catalog/IMAGE_DIGEST.md

The H2 catalog tier's image (ADR-003's `catalog` tier; question 40: "the catalog stays on
Postgres"). Pulled ONCE at setup, by digest, on 2026-09-15 (heavy track round 1). Question
154: the pull is the one-time setup step; no test ever pulls. Question 212(a): every image a
test depends on records its digest here, and the test compares the RUNNING image to this
record before it trusts it.

## Image

- Registry reference (pull by digest, never by the tag alone):
```
imresamu/postgis@sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd
```
- Tag this digest carried when it was pulled: `imresamu/postgis:17-3.5-alpine`
- `docker image inspect --format '{{.Id}}'` on this host:
```
sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd
```
- Architecture: `arm64`
- Measured inside the running container (2026-09-15):
  - `SHOW server_version` -> `17.11`
  - `SELECT postgis_full_version()` -> `POSTGIS="3.5.4 2fc837e" [EXTENSION] PGSQL="170"
    GEOS="3.14.1-CAPI-1.20.5" PROJ="9.7.1 NETWORK_ENABLED=OFF" ... TOPOLOGY`
  - The image's own init scripts already `CREATE EXTENSION postgis` in the default database,
    so the catalog's first migration says `CREATE EXTENSION IF NOT EXISTS postgis`, never a
    bare `CREATE EXTENSION` (measured: a bare one fails with "duplicate key value violates
    unique constraint pg_extension_name_index").

## Why this repository and not `postgis/postgis`

The official `postgis/postgis` images publish **no linux/arm64 manifest** (measured on this
host, 2026-09-15: `docker manifest inspect postgis/postgis:17-3.5` lists `amd64` plus the
buildkit `unknown` attestation entry and nothing else), and this host is arm64 Colima.

Building our own on top of the official `postgres:17-alpine` was tried first and **does not
work**, for a reason worth recording so nobody tries it again: the official `postgres` image
compiles PostgreSQL into `/usr/local`, while Alpine's own `postgis` apk package depends on
Alpine's `postgresql18` package and installs its extension into `/usr/share/postgresql18/
extension` against the pg18 ABI. The two never meet; `CREATE EXTENSION postgis` cannot find
the extension at all. (Build log, verbatim: "Setting postgresql18 as the default version".)

`imresamu/postgis` is the multiarch mirror of the official PostGIS Docker images maintained
by one of the PostGIS docker-postgis maintainers. It is pinned **by digest** here, which is
what question 212(a) asks for regardless of namespace: a test compares the running image to
this exact digest before it trusts it, so a namespace change upstream cannot silently alter
what a test ran against.

**H7 note for the P5 kit:** on an x86_64 production host, use the official
`postgis/postgis:17-3.5-alpine` and record ITS digest in the suite fragment; the arm64
mirror is a development-host accommodation, not a deployment decision.

## Running it (tests do this themselves, under the host-wide docker lock)

```
docker run -d --label av.test=1 --label av.test.run_id=<id> \
  -e POSTGRES_PASSWORD=<password> -e POSTGRES_DB=<db> \
  -p 127.0.0.1::5432 \
  imresamu/postgis@sha256:f8a700accce9a1fb24e14b73a3abf8e90439ce8f192c57b8cb93cc650694e8dd
```

No bind mount, for the same Colima reason `services/store/IMAGE_DIGEST.md` records.

**Auth, measured:** the image's generated `pg_hba.conf` ends with
`host all all all scram-sha-256`, so a client connecting from outside the container (which
is every client this workspace has) MUST perform SASL `SCRAM-SHA-256`. `trust` applies only
to `local`/`127.0.0.1` *inside* the container. This is why `crates/av-catalog`'s hand-rolled
wire client implements SCRAM-SHA-256 (on the system OpenSSL) rather than assuming trust.
