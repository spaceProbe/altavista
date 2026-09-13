//! Requirement 4 (question 202's charter; question 155's rule applied verbatim to this
//! service): binding the plaintext `EdgeIngest` server to a non-loopback address returns
//! the typed error and never opens a socket; binding to `127.0.0.1:0` works.
//!
//! Mirrors `crates/av-kernel/src/drm/binding.rs`'s own test names for the identical
//! property, applied here to `av_ingest::server::bind_loopback` (the wire's own bind
//! entry point) rather than the kernel's lockstep-endpoint refusal.

use av_ingest::server::{bind_loopback, BindError};

#[tokio::test]
async fn bind_loopback_refuses_a_non_loopback_address_and_never_opens_a_socket() {
    // Port 1 is privileged (root-only) on every POSIX host this test runs on, and
    // 0.0.0.0:1 is additionally non-loopback -- if `bind_loopback` actually attempted
    // `TcpListener::bind` here, a non-root process would get a permission-denied `Io`
    // error, not `NonLoopback`. Getting back `NonLoopback` specifically is therefore
    // direct evidence the bind syscall was never attempted at all (in addition to the
    // source itself: `av_ingest::server::bind_loopback` checks `is_loopback_address`
    // before its one and only call to `TcpListener::bind`).
    let err = bind_loopback("0.0.0.0:1").await.unwrap_err();
    assert!(matches!(err, BindError::NonLoopback { .. }), "{err:?}");

    // A second, unprivileged non-loopback port, for good measure -- the refusal must not
    // be an artefact of port 1 specifically.
    let err = bind_loopback("0.0.0.0:0").await.unwrap_err();
    assert!(matches!(err, BindError::NonLoopback { .. }), "{err:?}");

    // A non-loopback hostname (question 155's own "no DNS resolution" rule: an
    // unrecognized hostname is refused, never resolved and then trusted).
    let err = bind_loopback("example.invalid:50080").await.unwrap_err();
    assert!(matches!(err, BindError::NonLoopback { .. }), "{err:?}");
}

#[tokio::test]
async fn bind_loopback_on_an_ephemeral_port_works_and_reports_the_assigned_port() {
    let listener = bind_loopback("127.0.0.1:0").await.expect("127.0.0.1:0 must bind");
    let addr = listener.local_addr().expect("a bound listener reports its own local address");
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_ne!(addr.port(), 0, "the OS must have assigned a real ephemeral port, not literal 0");

    // A second, independent bind to another ephemeral loopback port must also succeed and
    // must land on a different port than the first (never a fixed port -- test hygiene).
    let second = bind_loopback("127.0.0.1:0").await.expect("a second 127.0.0.1:0 bind must also succeed");
    let second_addr = second.local_addr().unwrap();
    assert_ne!(addr.port(), second_addr.port(), "two ephemeral binds must not collide on the same port");
}

#[tokio::test]
async fn bind_loopback_recognizes_localhost_and_ipv6_loopback_spellings() {
    for address in ["localhost:0", "[::1]:0"] {
        bind_loopback(address).await.unwrap_or_else(|e| panic!("{address:?} must be accepted as loopback: {e:?}"));
    }
}
