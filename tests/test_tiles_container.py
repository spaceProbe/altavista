"""Heavy track, round 3, task P4: `docs/heavy-plan.md` H4's remaining round-2 open item 6 --
"a docker-gated proof of av-tiles serving out of a real MinIO. The store-backed `ObjectSource`
exists and is what the binary runs on; only the container-backed test is missing" -- built out
this round to prove the GATEWAY itself running as a container from a scripted, digest-recorded
image (`services/tiles/Dockerfile` + `services/tiles/build-image.sh` + `services/tiles/
IMAGE_DIGEST.md`, the identical shape `services/edge-plugin/` already established, for the
identical reason -- see that Dockerfile's own header comment, "Why this Dockerfile has no Rust
builder stage": this workspace's `spoore-cdm` path dependency makes a plain `docker build`
unable to resolve the manifest at all, regardless of which crate is actually being built).

# Why a Python test, not a Rust integration test

`crates/av-jobs/tests/store_tiler.rs` already proves the store-backed seam this task's own brief
cites ("`ObjectSource` exists ... `store_tiler.rs` already proves that seam end to end") from
Rust, against a real MinIO container, using `av_lockstep::docker` for the container lifecycle.
What is missing, and what this file adds, is the GATEWAY PROCESS ITSELF running as a container --
a Docker build-and-run concern, not a library-seam concern. Every existing docker-gated proof of
"a scripted, digest-recorded image, run as a container, with its own hardening asserted" in this
workspace (`tests/test_edge_plugin_container.py`, `tests/test_proposer_container.py`) is a Python
test using `subprocess`/`docker` directly, sharing one host-wide lock
(`altavista.docker_test_lock`) and one label convention (`altavista.container_hardening`) neither
language-specific to Rust nor naturally expressed as a `cargo test` (there is no Rust crate whose
job is "orchestrate Docker containers for a test" other than `av-lockstep`, which is itself a
dev-dependency of individual crates, not a place to bolt on "and also build+run a sibling
service's own container image"). Following that precedent keeps this proof in the same language,
sharing the same lock/label/hardening-assertion machinery, as every other container-lifecycle
test in this repository -- not a second, Rust-flavoured copy of the same reasoning.

# What this file reuses, and what it adds

`tests/heavy_stack.py` already has everything except the container itself: `rust_bins` (builds
`av-tile-fixture`), `minio` (a real, labelled MinIO container), `key_prefix`/`tile_set_label`,
`tile_set` (a real tile set built against that MinIO by `av-tile-fixture`). This round added
`tiles_gateway_container` to that same module -- the containerised sibling of `av_tiles_service`
(the existing host-subprocess gateway `tests/test_viewer_tiles_route.py` already uses), deliberately
a plain `@contextlib.contextmanager` rather than a `@pytest.fixture` (see its own doc for why:
this file's own "the container is gone afterward" assertion needs to run, within the SAME test,
after that context manager's own teardown) -- and `TILES_SKIP_REASON`/
`_parse_tiles_image_digest_md` beside `SKIP_REASON`'s own identical MinIO gate. See
`tests/heavy_stack.py`'s own module doc section on both for the full reasoning (the `--bind
0.0.0.0` trap, the `$HOME`-rooted scratch issuer, the no-writable-volume hardening story) -- not
repeated a second time here.

This file itself only holds the actual proof, in ONE test function (mirroring `tests/
test_edge_plugin_container.py`'s own single-big-test shape, `ResourceGuard.cleanup()` then
`assert_nothing_left()` both inside it): fetch a real tile and check its bytes against the
gateway's own `ETag`, fetch the manifest and check its bytes against the hash the tile set is
addressed by, get a 403 for a below-clearance token and read the refusal back off the gateway's
own admin counter, assert the hardening posture from BOTH `docker inspect` and `docker exec` of
the running container, and -- once the `with` block exits and the container has actually been
torn down -- confirm it is gone.

# Docker-gated, in this workspace's exact discipline (question 194)

A module-level skip reason is already computed at import time inside `tests/heavy_stack.py`
(`SKIP_REASON` for MinIO, `TILES_SKIP_REASON` for `av-tiles:local`, the latter checked against
its own recorded digest per question 212(a)); this file's own gate is their combination, printed
as one reason so `-rs` output names BOTH possible causes rather than only the first one checked.
The host-wide docker-test lock (`altavista.docker_test_lock.lock_docker_tests`), label-based
prune-before-create, and digest-before-trust discipline all live in the fixtures/context manager
this file imports, not duplicated here.

# What this test would catch (this task's own standing review requirement -- "for each test,
name the wrong implementation it would fail against")

* The manifest assertion would fail against a container whose `--store-endpoint`/credentials
  were silently swapped for a different bucket or prefix -- the SHA-256 the tile set was
  addressed by would not match what a wrong backend returned.
* The tile/`ETag` assertion would fail against a gateway that served a tile from the wrong
  address, or computed its `ETag` from something other than the actual returned bytes.
* The 403+counter assertion would fail against a gateway that let a below-clearance token
  through (wrong status), or against one whose refusal never reached the counter it claims to
  increment -- this task's own "a refusal that is not counted is not a refusal this project
  accepts" rule, read from the container's OWN admin surface, not inferred from the response.
* The hardening assertions would fail against a Dockerfile that dropped non-root `USER`,
  `--read-only`, `--cap-drop ALL`, or `--security-opt no-new-privileges` -- from BOTH
  `docker inspect` (fixed at create time) and `docker exec` (the kernel's own live view), so a
  flag that LOOKS passed but is not actually enforced (mismatched inspect vs. exec) would be
  caught by whichever vantage point actually disagrees.
* The final "container is gone" assertion would fail against a context manager whose own
  teardown leaked the container.

# No network at test time (question 154) / no environment mutation (question 199)

Identical restatement to `tests/heavy_stack.py`'s own module doc: binding/connecting to
`127.0.0.1` (and to a container published there) never leaves the host's own kernel network
stack. No test here calls `os.environ[...]`/`monkeypatch.setenv`; every setting reaches the
container as a `docker run` CLI argument or a bind-mounted file this test itself wrote.
"""
from __future__ import annotations

import hashlib
import subprocess

import httpx
import pytest

from altavista.container_hardening import assert_exec_hardening, assert_inspect_hardening
from altavista.pb.altavista.v1 import heavy_pb2
from heavy_stack import (  # noqa: F401 -- rust_bins/minio/tile_set are transitive fixture deps
    SKIP_REASON,
    TILES_SKIP_REASON,
    _valid_claims,
    admin_counter,
    key_prefix,
    minio,
    minio_bucket,
    rust_bins,
    tile_set,
    tile_set_label,
    tiles_gateway_container,
)

COMBINED_SKIP_REASON = SKIP_REASON or TILES_SKIP_REASON

REFUSAL_COUNTER_CODE = "tiles_layer_label_over_clearance"


@pytest.mark.skipif(COMBINED_SKIP_REASON is not None, reason=COMBINED_SKIP_REASON or "")
def test_tiles_gateway_container_serves_a_real_tile_counts_a_refusal_and_is_removed(
    rust_bins, minio, key_prefix, tile_set_label, tile_set
):
    with tiles_gateway_container(rust_bins, minio, key_prefix, tile_set_label) as gateway:
        issuer = gateway.issuer

        # Minted directly against this context manager's OWN scratch-rooted issuer (see its own
        # doc for why: its public key was bind-mounted into the container, so it is a different
        # keypair from heavy_stack.py's module-level `issuer` fixture, which is never
        # bind-mounted anywhere).
        token_at_clearance = issuer.mint(_valid_claims("p4-container-at-clearance", ["tile-readers"]))
        token_below_clearance = issuer.mint(_valid_claims("p4-container-below-clearance", ["tile-guests"]))

        # -------------------------------------------------------------------------------------
        # 1. The manifest: fetched through the containerised gateway, real RS256 token, its
        #    bytes' SHA-256 equal to the hash the tile set is addressed by (never
        #    re-encoded/reconstructed).
        # -------------------------------------------------------------------------------------
        manifest_resp = httpx.get(
            f"http://{gateway.endpoint}/v1/tilesets/{tile_set.manifest_sha256}/manifest",
            headers={"Authorization": f"Bearer {token_at_clearance}"},
            timeout=10.0,
        )
        assert manifest_resp.status_code == 200, manifest_resp.text
        assert hashlib.sha256(manifest_resp.content).hexdigest() == tile_set.manifest_sha256, (
            "the containerised gateway's own manifest bytes must hash to exactly the tile set's own identity"
        )

        manifest = heavy_pb2.TileSetManifest()
        manifest.ParseFromString(manifest_resp.content)
        assert manifest.object_key_prefix == key_prefix
        assert len(manifest.tiles) == tile_set.tile_count
        tile = manifest.tiles[0]

        # -------------------------------------------------------------------------------------
        # 2. A real tile: its bytes' SHA-256 equals the gateway's own ETag.
        # -------------------------------------------------------------------------------------
        tile_path = f"/v1/tilesets/{tile_set.manifest_sha256}/tiles/{tile.level}/{tile.x}/{tile.y}"
        tile_resp = httpx.get(
            f"http://{gateway.endpoint}{tile_path}",
            headers={"Authorization": f"Bearer {token_at_clearance}"},
            timeout=10.0,
        )
        assert tile_resp.status_code == 200, tile_resp.text
        etag = tile_resp.headers.get("etag", "").strip('"')
        assert etag, tile_resp.headers
        assert hashlib.sha256(tile_resp.content).hexdigest() == etag, "the tile's own bytes must hash to exactly the gateway's own ETag"
        assert etag == tile.sha256

        # -------------------------------------------------------------------------------------
        # 3. A below-clearance token gets 403, and the gateway's OWN refusal counter -- read
        #    back through its admin surface, not assumed -- increments by exactly 1.
        # -------------------------------------------------------------------------------------
        before = admin_counter(gateway, REFUSAL_COUNTER_CODE)

        refusal_resp = httpx.get(
            f"http://{gateway.endpoint}{tile_path}",
            headers={"Authorization": f"Bearer {token_below_clearance}"},
            timeout=10.0,
        )
        assert refusal_resp.status_code == 403, refusal_resp.text
        assert refusal_resp.content == b"", "a refusal must never carry tile bytes"

        after = admin_counter(gateway, REFUSAL_COUNTER_CODE)
        assert after == before + 1, (
            f"the containerised gateway's own {REFUSAL_COUNTER_CODE!r} counter must increment by "
            f"exactly 1 (before={before}, after={after}) -- a refusal that is not counted is not "
            f"a refusal this project accepts"
        )

        # -------------------------------------------------------------------------------------
        # 4. Hardening, from BOTH vantage points -- `docker inspect` (fixed at create time) and
        #    `docker exec` of the RUNNING container (the kernel's own view). No writable-volume
        #    argument at all: services/tiles/Dockerfile declares none, and av-tiles' own
        #    production code (grepped) writes nothing to disk -- see tiles_gateway_container's
        #    own doc.
        # -------------------------------------------------------------------------------------
        inspect_info = assert_inspect_hardening(gateway.container_name)
        exec_facts = assert_exec_hardening(gateway.container_name)

        # Question 148: an exit code is not evidence -- print what was actually observed.
        print(
            f"\n--- av-tiles container, served for real (question 148) ---\n"
            f"manifest: tiles={len(manifest.tiles)} sha256={tile_set.manifest_sha256}\n"
            f"tile: level={tile.level} x={tile.x} y={tile.y} etag={etag}\n"
            f"refusal counter {REFUSAL_COUNTER_CODE!r}: before={before} after={after}\n"
            f"docker inspect hardening: Config.User={inspect_info['Config']['User']!r} "
            f"HostConfig.ReadonlyRootfs={inspect_info['HostConfig']['ReadonlyRootfs']!r} "
            f"HostConfig.CapDrop={inspect_info['HostConfig'].get('CapDrop')!r} "
            f"HostConfig.SecurityOpt={inspect_info['HostConfig'].get('SecurityOpt')!r}\n"
            f"docker exec hardening: {exec_facts}"
        )

        container_name = gateway.container_name

    # -----------------------------------------------------------------------------------------
    # 5. The `with` block has now exited -- `tiles_gateway_container`'s own `finally` clauses
    #    (`docker rm -f`, `docker network rm`) have already run. Confirm the container is
    #    actually gone, the same way `tests/test_edge_plugin_container.py::ResourceGuard.
    #    assert_nothing_left` does: request it by its own exact name and require Docker to say
    #    it no longer exists, rather than merely trusting the cleanup call's own exit code
    #    (question 148: an exit code is not evidence).
    # -----------------------------------------------------------------------------------------
    result = subprocess.run(["docker", "inspect", container_name], capture_output=True, text=True, timeout=15)
    assert result.returncode != 0, (
        f"container {container_name!r} should have been removed when tiles_gateway_container's "
        f"own `with` block exited, but `docker inspect {container_name}` still succeeded:\n{result.stdout}"
    )
    assert "No such object" in result.stderr, f"expected docker's own 'No such object', got: {result.stderr!r}"
    print(f"\n--- container gone after teardown (question 148) ---\ndocker inspect {container_name}: {result.stderr.strip()}")
