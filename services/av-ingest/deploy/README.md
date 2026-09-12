# services/av-ingest/deploy

`nginx-av-ingest-grpc.conf.template` -- the TLS+mTLS front for `crates/av-ingest`'s
plaintext gRPC (question 202, ADR-004). See the template file's own header comment for why
this exists, what each `__PLACEHOLDER__` is and who (`secdeploy`, eventually; `tests/
test_edge_ingest_mtls.py`, today) fills it, and -- most importantly -- the
`ssl_verify_client optional_no_ca` decision and what it buys: nginx terminates TLS and
forwards whatever certificate was presented (trusted, untrusted, expired, or none), but
`crates/av-ingest`'s own `Announce` handler is the one place that decides accept or refuse,
so a wrong-CA or lapsed identity lands in `IdentityCounters`/`EvidenceResponse`
(`GetEvidence`/`GET /admin/api/evidence`), not only in this front's own access/error log.
(An earlier draft used plain `ssl_verify_client optional`, which `tests/
test_edge_ingest_mtls.py` caught failing the TLS handshake itself -- with a plain 400,
exactly like `ssl_verify_client on` -- the moment ANY certificate is presented but fails to
verify; `optional_no_ca` is nginx's own directive for "verify, but don't let a failure abort
the connection".)

Not valid nginx syntax on its own -- it is rendered (placeholders substituted) before use.

`tests/test_edge_ingest_mtls.py` is the test that renders and runs it today: it provisions
(or skips visibly without) a real seccert+lego CA, builds and starts a real `crates/
av-ingest/src/bin/av-ingest-server.rs` subprocess, renders this template with that CA's
material and a set of ephemeral loopback ports, runs `nginx -t` before ever starting nginx,
and then proves -- through the real, running nginx front, never by calling `crates/
av-ingest` in process -- that a valid seccert-issued leaf is accepted, a leaf from a
different CA is refused and counted as `issuer_not_trusted`, a lapsed leaf is refused and
counted as `expired`, and a request with no client certificate at all is refused by
`av-ingest` itself (not by nginx) -- consistent with the `ssl_verify_client optional_no_ca`
decision above. It also measures directly that `grpc_set_header`, not `proxy_set_header`,
is the directive a `grpc_pass` location actually honours.
