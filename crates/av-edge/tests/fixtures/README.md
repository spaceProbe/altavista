# Test fixtures -- throwaway keys only

`test_signing_key.pem` and `test_signing_key.pub.pem` are a P-384 (secp384r1) EC key pair
generated once for this crate's tests with:

```
openssl ecparam -name secp384r1 -genkey -noout -out test_signing_key.pem
openssl ec -in test_signing_key.pem -pubout -out test_signing_key.pub.pem
```

**This is not an identity.** It is not issued by any CA (seccert or otherwise), it signs
nothing outside this crate's own `tests/`, and it must never be reused as a real
producer's signing key, a seccert leaf, or any other identity in this platform. It exists
only so `tests/golden.rs` (and friends) have a fixed key pair to sign a fixed
`MeasurementBatch` against and commit the resulting signature bytes as a golden value --
ECDSA signatures are not byte-reproducible run to run (OpenSSL's ECDSA uses a random
nonce, not RFC 6979), so "signed once, committed, and asserted to verify forever" is the
only kind of golden a P-384 signature can be. See `docs/edge-plan.md` milestone E1 and
`crates/av-edge/src/lib.rs`'s module doc for why.
