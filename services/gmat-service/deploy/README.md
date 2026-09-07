# services/gmat-service/deploy

`nginx-gmat-grpc.conf.template` -- the TLS+mTLS front for this service's plaintext gRPC
(M5.3, ADR-004). See the parent `services/gmat-service/README.md`'s "TLS front door" and
"FIPS and crypto accounting" sections for why this exists and what it does/doesn't cover,
and the template file's own header comment for what each `__PLACEHOLDER__` is and who
(`secdeploy`, eventually; `tests/test_grpc_tls.py`, today) fills it.

Not valid nginx syntax on its own -- it is rendered (placeholders substituted) before use.
`tests/test_grpc_tls.py` is the only thing that renders and runs it today.
