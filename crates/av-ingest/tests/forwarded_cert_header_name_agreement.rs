//! `av_ingest::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER` (the canonical definition --
//! see that module's own doc comment) and `av_ingest_client::forwarded_cert::
//! FORWARDED_CLIENT_CERT_HEADER` (a second copy of the same literal, duplicated only
//! because `av-ingest-client` cannot depend on `av-ingest` itself -- this crate's own
//! tests already depend on `av-ingest-client`, and a dependency the other way would
//! cycle) must never drift apart. This is the one guard against that.

#[test]
fn the_client_and_server_crates_name_the_same_forwarded_client_cert_header() {
    assert_eq!(
        av_ingest::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER,
        av_ingest_client::forwarded_cert::FORWARDED_CLIENT_CERT_HEADER,
        "crates/av-ingest and crates/av-ingest-client must name the exact same forwarded-client-certificate gRPC metadata header"
    );
}
