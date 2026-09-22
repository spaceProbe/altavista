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
          "iat": now, "exp": now + 3600, "groups": ["tile-readers"], "amr": [], "acr": "",
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
clearance.

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

### 3. Start the viewer server against that gateway

```sh
.venv/bin/python -m altavista serve \
  --host 127.0.0.1 --port 18090 \
  --tiles-endpoint "127.0.0.1:18080" \
  --tiles-token-path "$WORKDIR/token.txt" &
```

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

### 5. Tear down

```sh
kill %1 %2                 # av-tiles, then the viewer server (job control, in the SAME shell)
rm -rf "$WORKDIR"
.venv/bin/python scripts/heavy/ten_gigabyte_proof.py --out-dir out/heavy-stackup-demo --teardown
```

## What this README does NOT cover

- The actual ten-gigabyte generation: run `ten_gigabyte_proof.py` at its own default shape
  (or a larger one) yourself when you are ready to spend the disk and the wall-clock time --
  this task's own author was instructed not to run it.
- `docs/heavy-plan.md`: owned by the manager; this file exists specifically so the exact
  commands above have a home that is NOT that document.
