# `scripts/heavy/` -- HEAVY track round 3, task P2a

## `ten_gigabyte_proof.py` -- the ten-gigabyte streaming proof

See that file's own module docstring (`python scripts/heavy/ten_gigabyte_proof.py --help`)
for the full design. Short version:

    .venv/bin/python scripts/heavy/ten_gigabyte_proof.py            # generate + verify, leaves the stack UP
    .venv/bin/python scripts/heavy/ten_gigabyte_proof.py --teardown # stop/remove everything, delete the tile set

Python, not shell, because it reuses three already-existing modules verbatim rather than
re-deriving them a second time (`altavista.container_hardening`, `altavista.docker_test_lock`,
`altavista.pb...heavy_pb2`) -- see the script's own module doc, "Why Python, not a shell
script".

**Do not run this at its default (multi-gigabyte) shape without checking free disk first** --
`--out-dir`'s default is `out/ten-gigabyte-tileset` under this worktree; the default shape
(`--tile-size 1024 --min-level 0 --max-level 6`) stores roughly 32 GiB, verified against a
smaller real run rather than trusted (see the script's own module doc for the exact
arithmetic and why the task brief's own suggested shape actually undershoots ten gigabytes).

## Standing up the stack for a browser drive (deliverable 3)

This is the exact, copy-pasteable sequence that brings up the store, the `av-tiles` gateway,
and the viewer server against a generated tile set, with a real bearer token, so a human can
open a browser at the viewer server's own URL and see the tile set stream. **Verified by
running every line below against a real, small (126 MB, 42-tile) tile set** produced by
`ten_gigabyte_proof.py` itself (see that command's own output for where each value below came
from) -- not against the ten-gigabyte shape, which is the manager's own measurement to run,
per this task's own binding rule ("Do not run the ten-gigabyte generation yourself").

### 0. Generate a tile set and leave the stack up

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
.venv/bin/python scripts/heavy/ten_gigabyte_proof.py \
  --out-dir out/heavy-stackup-demo \
  --tile-size 1024 --min-level 0 --max-level 2 \
  --job-id heavy-stackup-demo
```

The script's own final lines name everything the rest of this README needs:

```
Stack is left UP: MinIO container <id> on 127.0.0.1:<MINIO_PORT>, bucket '<BUCKET>', key_prefix '<KEY_PREFIX>'.
manifest_sha256 = <MANIFEST_SHA256>          # <- THIS is where the manifest hash comes from: it is
                                              #    printed by av-tile-fixture itself (JSON field
                                              #    "manifest_sha256"), echoed by this script, and it is
                                              #    the tile set's own content-addressed identity --
                                              #    never chosen or guessed by a caller.
```

(Round 7 task 5a: `ten_gigabyte_proof.py` itself has no `--synthetic-source-style` pass-through
flag of its own -- it always builds the default `gradient` raster internally, unaffected by
this task either way. Step 0.5c below is where this recipe's own `--synthetic-source-style
labelled` variant lives, because it is the only step that registers anything the browser drive
can actually see -- see that step's own note.)

The MinIO container's own access key / secret key are not printed by the proof script (they
never need to be, for that script's own purposes -- it holds them in memory and uses them
itself); read them back off the running container when you need them for the next step:

```sh
CONTAINER_ID=<the id printed above>
docker inspect "$CONTAINER_ID" --format '{{range .Config.Env}}{{println .}}{{end}}' | grep MINIO_ROOT
# MINIO_ROOT_USER=...
# MINIO_ROOT_PASSWORD=...
docker port "$CONTAINER_ID" 9000/tcp
# 127.0.0.1:<MINIO_PORT>
```

### 0.5. Register the generated tile set in the catalog (round 5, question 228 finding 2)
**-- steps a/b/c below WERE RUN FOR REAL this round (heavy round 6, task 5).** Round 5 left
step b as a real, named gap -- `av_catalog::migrate::Migrator::apply_pending` was a LIBRARY
function with no standalone CLI anywhere in this workspace (`docs/heavy-plan.md`'s "A gap in
the drive path that nobody had noticed" section), so step b used to be a paragraph explaining
that gap rather than a command. It is now `av-catalog-migrate`
(`crates/av-catalog/src/bin/av-catalog-migrate.rs`, this round's own new `[[bin]]` target of
the `av-catalog` crate -- see that file's own module doc for why a `[[bin]]` of THIS crate,
not `cargo run --example`, not a new workspace member). **What "run for real" means here:**
this task ran `services/catalog/run-dev-catalog.sh` (step a), `av-catalog-migrate` against the
real container it started (step b, both a first schema-applying run and a second, idempotent
one), and `av-tile-fixture --catalog-*` against the now-migrated database (step c) -- this
task's own report quotes every one of those runs' real output. The exact commands below, with
the substituted values, were not re-typed a second time by a human this round (they are
derived from the real, on-disk flag parsers the same way step 0.5c's flags always were, per
this section's own pre-existing standard) -- what proves this whole sequence works, every time,
committed and re-runnable by anyone, is
`tests/test_catalog_tilesets_route.py::test_real_round_trip_migrate_then_register_then_list_through_the_real_gateway_route`
(docker-gated, real PostGIS + real MinIO + real `av-catalog-migrate` + real `av-tile-fixture`
+ a real `av-gateway` subprocess + a real `GET /api/catalog/tilesets` through `create_app`):

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
.venv/bin/python -m pytest tests/test_catalog_tilesets_route.py -k real_round_trip -q -s
```

This is the only step that makes a generated tile set visible to `GET /api/catalog/tilesets`
(and therefore to the Layers panel) -- step 0 above never registers anything; it only writes
tile objects into the MinIO store. It needs a real PostGIS catalog reachable
(`services/catalog/IMAGE_DIGEST.md` -- the image is present, by digest, on this host this
round) and belongs after step 0 (it needs step 0's own `manifest_sha256`/`key_prefix`, and it
re-runs the exact same `av-tile-fixture` binary step 0 already used) and before the browser is
opened.

```sh
# a. Start the real, digest-verified PostGIS catalog container (a developer convenience
#    script, never invoked by a test -- see that script's own header comment). Run for real
#    this round: prints e.g. "PostGIS dev container started (image id sha256:f8a700ac...,
#    matches .../IMAGE_DIGEST.md)." plus the container id, host port, user, password, database.
services/catalog/run-dev-catalog.sh
# prints the container id and the host port it published 5432/tcp on, e.g.:
#   Connect:      psql -h 127.0.0.1 -p <CATALOG_PORT> -U postgres -d <CATALOG_DATABASE>

# b. Apply the catalog schema -- av-catalog-migrate, this round's own new binary. Idempotent: a
#    second run against the same, already-migrated database applies nothing and says so (run
#    for real this round, both ways -- see this task's own report for the exact quoted output
#    of both runs). --applied-tai-ns is optional; the documented default (never a silent read)
#    is the real OS wall clock, converted UTC -> TAI via av_cdm::time::Tai::from_utc_nanos --
#    pass the flag yourself to pin an exact value instead.
#    Lead's drive (2026-09-22): run this a few seconds AFTER step a returns, or retry it.
#    Docker publishes the host port the instant the container starts, before Postgres
#    inside it listens (about 1.8 s, measured in heavy round 6), so an immediate run fails
#    with "connection closed by peer while reading startup/authentication" -- not a
#    schema or credential problem, just too early.
cargo build -p av-catalog --bin av-catalog-migrate
target/debug/av-catalog-migrate \
  --catalog-host 127.0.0.1 --catalog-port "<CATALOG_PORT from a.>" \
  --catalog-user postgres \
  --catalog-password "<CATALOG_PASSWORD from a.>" --catalog-database "<CATALOG_DATABASE from a.>"
# prints what schema_migrations already recorded before this run, the applied_tai_ns used and
# where it came from, exactly which migrations THIS run applied (or "no pending migrations --
# database already at the latest schema version (idempotent: this run applied nothing)" on a
# second run), and the resulting schema version.

# c. Re-run av-tile-fixture (cargo build -p av-jobs --bin av-tile-fixture --features
#    store-fixture, the SAME binary and the SAME --store-*/--key-prefix/--job-id values step
#    0's ten_gigabyte_proof.py used internally -- content-addressed, so re-running it against
#    the identical inputs is idempotent, never a second, diverging tile set), this time WITH
#    the catalog flags so it ALSO registers the manifest as a CatalogAsset. Run for real this
#    round (against the automated test's own smaller synthetic tile set, not this exact
#    heavy-stackup-demo job -- see this task's own report): prints "registered the tile-set
#    manifest in the catalog (asset_id=...)" on success, or a real, typed PostgreSQL error
#    (e.g. `42P01 relation "assets" does not exist`) if step b above was skipped.
#    Lead's drive (2026-09-22): step 0's script builds this binary in RELEASE
#    (target/release/av-tile-fixture), and the flag parser requires exactly one of
#    --source-path / --synthetic-source -- step 0 used --synthetic-source 64x32, so pass the
#    same here or the fixture refuses with its usage line before doing anything.
target/release/av-tile-fixture \
  --key-prefix heavy-stackup-demo --ladder "UNCLASSIFIED,CUI,SECRET" --label-marking CUI \
  --job-id heavy-stackup-demo --min-level 0 --max-level 2 --tile-size 1024 \
  --synthetic-source 64x32 \
  --store-endpoint "http://127.0.0.1:<MINIO_PORT>" --store-region us-east-1 \
  --store-access-key-id "<MINIO_ROOT_USER>" --store-secret-access-key "<MINIO_ROOT_PASSWORD>" \
  --store-bucket "<BUCKET>" --store-path-style \
  --catalog-host 127.0.0.1 --catalog-port "<CATALOG_PORT from a.>" \
  --catalog-user postgres \
  --catalog-password "<CATALOG_PASSWORD from a.>" --catalog-database "<CATALOG_DATABASE from a.>"
# (flag names verified at crates/av-jobs/src/bin/av-tile-fixture.rs lines ~192-197/274-279, and
# run for real this round -- see this task's own report)
```

**Round 7 task 5a: make the switch visible with `--synthetic-source-style labelled`.** The
command above is byte for byte what it always was, and still produces exactly the same
`gradient` raster -- `--synthetic-source-style` is a new, OPT-IN flag, and every command in
this file that omits it (including the one above) is completely unaffected by this task. The
gradient raster reads as a smooth teal/brown-ish ramp that looks close enough to the offline
fixture Earth texture's own teal/brown palette that a person driving the browser could not
actually SEE the switch step 0.5's own registration proves happened. This step is where that
switch becomes visible, because it is the only step in this whole recipe that ever registers a
manifest in the catalog (step 0 above writes tiles to MinIO but registers nothing) -- so
whichever style this command renders is the entire tile set the Layers panel ever lists.

Pass `--synthetic-source-style labelled` on the SAME command to render a high-contrast
magenta/black checkerboard with a baked lat/lon grid and the word `SYNTHETIC` stamped on it
instead (`crates/av-jobs/src/bin/av-tile-fixture.rs`'s own module doc, "round 7 task 5a", has
the exact colours/formula and why per-tile `level/x/y` text specifically cannot be baked in).
Because this changes the raster's own bytes, it is deliberately NOT the same content-addressed
re-registration the paragraph above describes: it performs a genuine second write (new tile
objects, a new manifest, both under the SAME `--key-prefix`/`--job-id`) and registers THAT
manifest instead. Nothing already recorded by digest moves -- step 0's own gradient-style
objects and manifest are untouched on MinIO, just no longer the one thing the catalog points
at:

```sh
target/release/av-tile-fixture \
  --key-prefix heavy-stackup-demo --ladder "UNCLASSIFIED,CUI,SECRET" --label-marking CUI \
  --job-id heavy-stackup-demo --min-level 0 --max-level 2 --tile-size 1024 \
  --synthetic-source 64x32 --synthetic-source-style labelled \
  --store-endpoint "http://127.0.0.1:<MINIO_PORT>" --store-region us-east-1 \
  --store-access-key-id "<MINIO_ROOT_USER>" --store-secret-access-key "<MINIO_ROOT_PASSWORD>" \
  --store-bucket "<BUCKET>" --store-path-style \
  --catalog-host 127.0.0.1 --catalog-port "<CATALOG_PORT from a.>" \
  --catalog-user postgres \
  --catalog-password "<CATALOG_PASSWORD from a.>" --catalog-database "<CATALOG_DATABASE from a.>"
```

Run ONE of the two variants above, not both, for a given drive -- whichever ran LAST is the one
the catalog (and therefore step 5's Layers panel) will show. Not run against the live stack
this round (this task's own report says exactly what WAS run: the real binary, real PNG tiles,
real manifest, all via `--dry-run`, plus every unit test in `av-tile-fixture.rs`'s own `mod
tests`) -- see that report for the measured per-tile byte cost of each style (identical to each
other, and to the gradient style's own -- this crate's hand-rolled PNG encoder never compresses,
so file size is a pure function of `--tile-size`, never of pixel content).

### 1. Mint a real RS256 bearer token (no new dependency: the system `openssl` CLI)

`av-tiles` verifies a real OIDC-shaped RS256 JWT against a configured public key -- there is
no "test mode" that skips this. `tests/heavy_stack.py::LocalTestIssuer` is the reusable
version of this recipe; the inline form below is the same four `openssl`/base64url steps, so
this README does not silently drift from what the test suite actually proves:

```sh
WORKDIR=$(mktemp -d)
openssl genrsa -out "$WORKDIR/issuer_private.pem" 2048
openssl rsa -in "$WORKDIR/issuer_private.pem" -pubout -out "$WORKDIR/issuer_public.pem"

.venv/bin/python - "$WORKDIR" <<'PYEOF'
import base64, json, subprocess, sys, time, uuid
from pathlib import Path
tmp = Path(sys.argv[1])
def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")
now = int(time.time())
claims = {"iss": "https://sso.test.example/", "aud": "av-tiles", "sub": "stackup-demo",
          "iat": now, "exp": now + 3600, "groups": ["tile-readers", "operators"], "amr": [], "acr": "",
          "jti": str(uuid.uuid4())}
header_b64 = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}, separators=(",", ":")).encode())
payload_b64 = b64url(json.dumps(claims, separators=(",", ":")).encode())
signing_input = f"{header_b64}.{payload_b64}"
proc = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(tmp / "issuer_private.pem")],
                       input=signing_input.encode(), check=True, capture_output=True)
token = f"{signing_input}.{b64url(proc.stdout)}"
(tmp / "token.txt").write_text(token)
print(f"token written to {tmp / 'token.txt'}")
PYEOF
```

`"groups": ["tile-readers"]` is what `--group-clearance tile-readers=CUI` (step 2) maps to
the tile set's own `CUI` label -- change both together if you mint a token for a different
clearance. `"operators"` is the group `profiles/gateway-authority.yaml` grants the `query`
surface to; without it step 2b's `av-gateway` answers step 4b with
`403 no role in groups ["tile-readers"] grants surface query` (lead's drive, 2026-09-22),
and the viewer's Layers panel lists nothing.

### 2. Start the real `av-tiles` gateway against that MinIO

```sh
cargo build -p av-tiles --bin av-tiles
target/debug/av-tiles \
  --oidc-issuer "https://sso.test.example/" \
  --oidc-audience "av-tiles" \
  --oidc-public-key-path "$WORKDIR/issuer_public.pem" \
  --ladder "UNCLASSIFIED,CUI,SECRET" \
  --key-prefix "<KEY_PREFIX from step 0>" \
  --store-endpoint "http://127.0.0.1:<MINIO_PORT>" \
  --store-region "us-east-1" \
  --store-access-key-id "<MINIO_ROOT_USER from step 0>" \
  --store-secret-access-key "<MINIO_ROOT_PASSWORD from step 0>" \
  --store-bucket "<BUCKET from step 0>" \
  --store-path-style \
  --group-clearance "tile-readers=CUI" \
  --bind "127.0.0.1:18080" \
  --admin-bind "127.0.0.1:18081" \
  --admin-role "stackup-admins" &
```

Wait for its own `av-tiles: LISTENING 127.0.0.1:18080` line on stdout before continuing (this
is the identical readiness signal `tests/heavy_stack.py::_wait_for_listening_line` polls for
-- never a fixed sleep).

**On `--admin-role` (round 5).** `GET /admin/api/counters` on the admin bind now
authenticates, exactly as `av-gateway`'s admin surface does (question 229's ruling;
`crates/av-tiles/src/admin.rs`). It is deny-by-default: with **no** `--admin-role` flag the
port binds and refuses every caller, so the flag is given here to keep this recipe's admin
port actually usable. Note the group is `stackup-admins`, **not** the `tile-readers` group
step 1's token carries -- an admin credential and a tile-clearance credential are
deliberately never the same token (`tests/heavy_stack.py` keeps the same separation). Step
1's token therefore gets `403` here, which is the correct answer, not a fault. To read the
counters, mint a second token with step 1's block changing only
`"groups": ["stackup-admins"]`, and send it as `Authorization: Bearer <token>`
(unlike step 4's response headers, this block is derived from the implementation and from
`tests/heavy_stack.py`'s equivalent fixture rather than pasted from a live run):

```sh
curl -sS -H "Authorization: Bearer $(cat "$WORKDIR/admin_token.txt")" \
  http://127.0.0.1:18081/admin/api/counters
```

### 2b. Start the real `av-gateway` DataGatewayService (round 5, question 228 finding 2)
**-- NOT RUN as this exact copy-pasted shell recipe this round either** (step 0.5's own reason
this used to cite -- "no PostGIS/catalog tier actually stood up" -- no longer applies; heavy
round 6 stood one up for real, see step 0.5's own banner). What WAS run for real this round is
an equivalent `av-gateway --catalog-*` invocation, inside
`tests/test_catalog_tilesets_route.py`'s own committed, docker-gated round-trip test -- see
that test and this task's own report. Flags verified against `crates/av-gateway/src/bin/av-gateway.rs`'s own CLI
parser (not merely its doc comment): `--oidc-issuer`/`--oidc-audience`/
`--oidc-public-key-path` are REQUIRED (R5.1) -- reuse step 1's SAME issuer/public key/ladder,
a second, independent token mint is not needed, `av-gateway` verifies against the same
`issuer_public.pem`. The bind address is an environment variable, `AV_GATEWAY_BIND`
(default `127.0.0.1:50071`), not a flag -- set explicitly here to avoid colliding with
`av-tiles`' own `18080`/`18081` and the viewer's `18090`.

```sh
cargo build -p av-gateway --bin av-gateway
AV_GATEWAY_BIND=127.0.0.1:18082 target/debug/av-gateway \
  --oidc-issuer "https://sso.test.example/" \
  --oidc-audience "av-tiles" \
  --oidc-public-key-path "$WORKDIR/issuer_public.pem" \
  --catalog-host 127.0.0.1 --catalog-port "<CATALOG_PORT from step 0.5a>" \
  --catalog-user "<same as step 0.5c>" --catalog-password "<same>" --catalog-database "<same>" &
```

(`--oidc-audience "av-tiles"` here deliberately reuses step 1's SAME token/audience -- both
services trust the identical issuer/audience/ladder in this recipe, matching
`altavista.gateway_client`'s own module doc: "caller_clearance is always sent empty ... the
token-derived clearance is used", so one real, valid token is sufficient for both proxied
surfaces. A deployment that wants `av-gateway` and `av-tiles` behind genuinely different
audiences would mint a second token here instead -- out of scope for this recipe.)

### 3. Start the viewer server against the tiles gateway (and, new this round, the data
gateway -- `--gateway-endpoint`/`--gateway-token-path`, `altavista/__main__.py`)

```sh
.venv/bin/python -m altavista serve \
  --host 127.0.0.1 --port 18090 \
  --tiles-endpoint "127.0.0.1:18080" \
  --tiles-token-path "$WORKDIR/token.txt" \
  --gateway-endpoint "127.0.0.1:18082" \
  --gateway-token-path "$WORKDIR/token.txt" &
```

The two new flags on the last line are unverified end-to-end this round (they depend on
step 2b, which was not run) -- but the flags themselves, and that this command starts and
serves every OTHER route normally with them present, ARE verified: every test in
`tests/test_viewer_layers_panel.py` (this task's own new pytest file) starts exactly this
binary with `--gateway-endpoint`/`--gateway-token-path` pointed at a real (fixture) gateway
and confirms `GET /api/health` still answers, the page still loads with zero exceptions, and
`GET /api/catalog/tilesets` answers for real.

Open `http://127.0.0.1:18090/` in a browser -- this is the URL the lead drives.

### 4. Prove the route works with `curl` before trusting the browser

Measured directly (real run, real headers, pasted verbatim below -- not retyped from memory):

```sh
MANIFEST=<manifest_sha256 from step 0>
curl -sS -D - -o /tmp/manifest.bin "http://127.0.0.1:18090/api/tiles/${MANIFEST}/manifest"
curl -sS -D - -o /tmp/tile.bin     "http://127.0.0.1:18090/api/tiles/${MANIFEST}/tiles/0/0/0"
```

Real response headers from exactly this sequence, run against a live stack while writing this
file (manifest_sha256 `1b089de7436c9d261d6a8afaca11dd632c6cd6f81e39180cd7f7080963b4ae0e`):

```
--- manifest route ---
HTTP/1.1 200 OK
date: Thu, 17 Sep 2026 06:26:47 GMT
server: uvicorn
etag: "1b089de7436c9d261d6a8afaca11dd632c6cd6f81e39180cd7f7080963b4ae0e"
cache-control: public, max-age=31536000, immutable
content-length: 7850
content-type: application/vnd.altavista.tileset-manifest+pb

--- tile route (level 0, x 0, y 0) ---
HTTP/1.1 200 OK
date: Thu, 17 Sep 2026 06:26:47 GMT
server: uvicorn
etag: "743c410282fb36a30b42e2a187722b8e0f56dadfef37081c2f28627fcf9d525b"
cache-control: public, max-age=31536000, immutable
content-length: 3147060
content-type: image/png
```

`shasum -a 256 /tmp/tile.bin` reproduced `743c410282fb36a30b42e2a187722b8e0f56dadfef37081c2f28627fcf9d525b`
-- the tile's own `etag`, byte for byte, exactly as `crates/av-jobs/tests/tiler.rs`'s and
`crates/av-jobs/tests/store_tiler.rs`'s own assertions require of every tile this pipeline
ever serves.

### 4b. Prove the catalog route with `curl`, the same "before trusting the browser" discipline

```sh
curl -sS -D - "http://127.0.0.1:18090/api/catalog/tilesets"
```

With step 2b actually up, expect `200` and `{"tileSets": [{"manifestSha256": "<MANIFEST
from step 0>", ...}, ...]}`. **Not run this round** (step 2b was not run) -- but the SAME
route, against a real (fixture) gateway, was measured by this task's own
`tests/test_viewer_layers_panel.py`: a real `200` with `{"tileSets": [...]}` when configured,
and a real `503` with body `{"detail": "no data gateway is configured for this viewer
server (pass --gateway-endpoint/--gateway-token-path to \`python -m altavista serve\`)"}`
when it is not -- verified by that test's own second case, which asserts the Layers panel
shows that exact message, never "No tile sets in the catalog."

### 5. Open the Layers panel and toggle the tile set on -- select it "as a user would"
**This exact sequence IS verified** (`tests/test_viewer_layers_panel.py::
test_selecting_a_catalogued_tile_set_from_the_layers_panel_requests_its_tiles`, a real
headless Chrome against a real running `python -m altavista serve` this task started with
these same two flags) -- against that test's own fixture gateway/tiles-gateway pair, not
against this README's own live stack (which needs step 0.5/2b, not run this round; see this
task's own report for the full "what was and was not run" account).

1. In the browser at `http://127.0.0.1:18090/`, open an empty pane's chooser (or a pane
   header's swap menu) and pick **"Layers"** -- `web/js/layout/default_layouts.js`'s
   `REGISTERED_PANEL_TYPES` makes it reachable from there in every layout/profile (it is
   NOT in any default layout -- see that file's own `LAYERS_PANEL_ID` comment for why, and
   this task's own report for the trade-off).
2. Click **"Refresh catalog"**. The panel fetches `GET /api/catalog/tilesets` for real and
   lists every tile set the gateway's catalog returned (name, marking, size, and the
   manifest sha256 abbreviated -- full value in that cell's own tooltip).
3. Check **"Tiled globe (Earth)"** in the sidebar if it is not already on -- the Layers panel
   registers its layer on the SAME shared `LayerManager` the globe uses
   (`viewer.layerManager`, `web/js/scene.js`), and that manager is only DRIVEN (`update()`
   called) once per tick while the globe or a 3D Tiles overlay is active (see
   `web/js/globe.js`'s own note and `web/js/tiles_layer.js`'s "Round 5" module docstring --
   this task deliberately did not add a third, independent driver).
4. Click **"Turn on"** next to the desired tile set. The row's own status cell shows
   "fetching manifest…" then "active"; the "Streaming budget" section (same panel, below the
   table) shows real resident-bytes-against-budget, deferred, and failed counts, refreshed
   about twice a second as the view streams.
5. Click **"Turn off"** to remove it -- `viewer.layerManager.removeLayer(...)`, releasing its
   resident bytes; toggling it back on again re-registers under the identical, stable,
   manifest-sha256-derived layer id with no collision.

### 6. Tear down

```sh
kill %1 %2 %3               # av-tiles, av-gateway (if step 2b was run), then the viewer server
                             # (job control, in the SAME shell -- adjust job numbers to match
                             # whichever of the three you actually started)
rm -rf "$WORKDIR"
.venv/bin/python scripts/heavy/ten_gigabyte_proof.py --out-dir out/heavy-stackup-demo --teardown
docker rm -f "<the PostGIS container id from step 0.5a, if it was started>"
```

## What this README does NOT cover

- The actual ten-gigabyte generation: run `ten_gigabyte_proof.py` at its own default shape
  (or a larger one) yourself when you are ready to spend the disk and the wall-clock time --
  this task's own author was instructed not to run it.
- `docs/heavy-plan.md`: owned by the manager; this file exists specifically so the exact
  commands above have a home that is NOT that document.
- **Round 5 (question 228 finding 2) additions -- steps 2b and 4b's live-stack half are STILL
  UNVERIFIED this round** as a copy-pasted shell recipe, clearly marked inline above. **Step
  0.5 (a/b/c) is no longer in that category: heavy round 6 (task 5) ran it for real**, closing
  the `av-catalog-migrate` gap step b used to describe -- see that step's own banner above,
  this task's own report for the quoted output, and
  `tests/test_catalog_tilesets_route.py::test_real_round_trip_migrate_then_register_then_list_through_the_real_gateway_route`
  for the committed, docker-gated proof that ALSO covers step 2b's `av-gateway` and step 4b's
  `GET /api/catalog/tilesets` for real (that one test's own scope is wider than its home
  section's "0.5" number suggests). Every flag every step in this file names was verified
  against the real, on-disk CLI parsers (never copied from a doc comment alone) -- see this
  task's own report for exactly what was and was not run this round, and
  `tests/test_viewer_layers_panel.py` for the equivalent proof of steps 3-5 against a real
  (fixture, not docker/cargo) gateway pair instead.
