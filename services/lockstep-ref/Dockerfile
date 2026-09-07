# services/lockstep-ref/Dockerfile (M15.3, docs/open-questions.md question 118)
#
# A real, runnable Docker image of the LockstepService reference fixture -- used only to prove
# out av-kernel's own Docker image lifecycle (pull by digest, run, Bind, Reset, Shutdown, stop,
# remove; see crates/av-lockstep/src/docker.rs and crates/av-kernel/src/drm/binding.rs). This is
# still the same test fixture crates/av-kernel/tests/drm_container.rs already spawns as a bare
# subprocess -- see this package's own README's "This is a test fixture, not a deployed
# service" section -- packaged as an image only so the *image lifecycle* half of question 118
# has something real to pull/run/stop/remove. No image built from this Dockerfile is ever
# pushed to a real/external registry (question 118's own rule); tests that need to prove a
# genuine "pull by digest" push it only to a throwaway, loopback-only local registry they start
# and stop themselves (see tests/test_lockstep_ref.py).
#
# Build from the REPOSITORY ROOT (not this directory) -- the image needs
# altavista/pb/altavista/v1's already-generated protobuf/gRPC bindings, which live outside this
# package:
#
#   docker build -f services/lockstep-ref/Dockerfile -t lockstep-ref:local .
#
# `altavista`'s own top-level `__init__.py` (altavista/__init__.py) is deliberately NOT copied in:
# it eagerly imports `gmat_env` (real GMAT bindings) that this reference process -- a pure
# SIGNAL-in/SIGNAL-out integrator with no GMAT dependency at all, see server.py's own module
# doc comment -- has no business needing inside a minimal container image. A one-line namespace
# stand-in is written instead so `altavista.pb.altavista.v1.*` still imports cleanly.
FROM python:3.13-slim

WORKDIR /app

# Pinned to match this repository's own .venv (`pip show grpcio protobuf`) exactly, so the
# generated *_pb2.py files' own gencode-version check never disagrees between "runs in the
# venv" and "runs in this image".
RUN pip install --no-cache-dir grpcio==1.83.1 protobuf==7.36.1

COPY altavista/pb /app/altavista/pb
RUN printf '"""Minimal namespace package -- altavista/__init__.py itself is not copied into this\nimage; see services/lockstep-ref/Dockerfile for why."""\n' > /app/altavista/__init__.py

COPY services/lockstep-ref/lockstep_ref /app/lockstep_ref

ENV PYTHONDONTWRITEBYTECODE=1
ENV PYTHONUNBUFFERED=1

EXPOSE 50070

# --host 0.0.0.0: this process must accept connections from outside its own network namespace
# for `docker run -p 127.0.0.1::50070` to reach it at all -- see __main__.py's serve() doc
# comment. The kernel's own connection is still loopback-only either way (ADR-003).
ENTRYPOINT ["python", "-m", "lockstep_ref", "--host", "0.0.0.0"]
