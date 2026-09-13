//! Binding the `EdgeIngest` plaintext socket: **loopback only** (question 155's rule,
//! applied verbatim to this service; question 202's charter). A non-loopback bind address
//! is refused at load with a typed error, before a socket is ever opened -- mirroring
//! `crates/av-kernel/src/drm/binding.rs::is_loopback_address`'s own refusal for the
//! kernel's plaintext lockstep endpoint (that function's own doc comment's reasoning is
//! reused here almost verbatim: no DNS resolution, so an unrecognized hostname is treated
//! as non-loopback and refused rather than resolved and then trusted, which would make the
//! refusal depend on the resolver's answer at load time instead of on the address string
//! itself).
//!
//! [`is_loopback_address`] is a second, independent copy of that same ~15-line function
//! rather than a shared dependency: this workspace has no existing "network address
//! utilities" crate either module could depend on without inventing one for a single
//! function, and `av-kernel`'s own copy is `pub(crate)` (never exported) for the same
//! reason. `tests/wire_bind_refusal.rs`'s own test names mirror
//! `crates/av-kernel/src/drm/binding.rs`'s test names for the same property, so a future
//! reader can compare the two directly.
use tokio::net::TcpListener;

/// Question 155/202: is `address` (a bare `"host:port"` string) a recognized loopback
/// endpoint? See this module's own doc comment; identical logic to
/// `crates/av-kernel/src/drm/binding.rs::is_loopback_address`; `port` is not inspected.
pub(crate) fn is_loopback_address(address: &str) -> bool {
    let host = match address.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => host,
        _ => address,
    };
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// What can go wrong turning a bind address into a listening plaintext socket.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// `address` did not parse into a recognized loopback endpoint (question 155/202) --
    /// refused before any socket was ever opened. A cross-host plugin must instead go
    /// through a service-owned nginx mTLS front (`docs/edge-plan.md` milestone E3b's own
    /// scope note; not rendered or run by this crate).
    #[error("{address:?} is not a loopback address -- refusing a plaintext bind (question 155/202: plaintext gRPC is loopback-only; a non-loopback endpoint needs a service-owned nginx mTLS front)")]
    NonLoopback { address: String },
    /// The address was a recognized loopback spelling, but the OS `bind`/`listen` call
    /// itself failed (e.g. the port is already in use, or -- for the `"localhost"`
    /// spelling -- the resolver could not resolve it).
    #[error("binding {address}: {source}")]
    Io { address: String, #[source] source: std::io::Error },
}

/// Binds a plaintext `TcpListener` to `address`, refusing (as [`BindError::NonLoopback`])
/// anything [`is_loopback_address`] does not recognize as loopback -- **before** ever
/// calling [`TcpListener::bind`], so a non-loopback address never opens a socket even
/// briefly. `address` is handed to [`TcpListener::bind`] as a bare string (not first
/// parsed into a `std::net::SocketAddr`) so the one hostname [`is_loopback_address`]
/// recognizes by its literal spelling -- `"localhost"` -- can still actually bind (`tokio`
/// resolves it the ordinary way any client dialling `"localhost"` would); this does not
/// reopen the "no DNS resolution" rule that function's own doc comment describes, since
/// that rule governs *deciding trust* from an arbitrary caller-supplied hostname, not
/// resolving the one already-trusted literal this function only reaches after the loopback
/// check has already passed. `"127.0.0.1:0"` (an ephemeral port, what every test in this
/// crate uses -- never a fixed port) works exactly like any other loopback spelling; the
/// OS-assigned port is read back from the returned listener's own `local_addr()`.
pub async fn bind_loopback(address: &str) -> Result<TcpListener, BindError> {
    if !is_loopback_address(address) {
        return Err(BindError::NonLoopback { address: address.to_string() });
    }
    TcpListener::bind(address).await.map_err(|source| BindError::Io { address: address.to_string(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_loopback_address_recognizes_the_documented_spellings() {
        for address in ["127.0.0.1:50070", "127.0.0.1", "127.1.2.3:1", "localhost:50070", "LOCALHOST:1", "[::1]:50070"] {
            assert!(is_loopback_address(address), "{address:?} should be recognized as loopback");
        }
    }

    #[test]
    fn is_loopback_address_refuses_everything_else() {
        for address in ["0.0.0.0:50070", "example.com:50070", "10.0.0.5:50070", "evil.internal:1", "[::]:1"] {
            assert!(!is_loopback_address(address), "{address:?} should NOT be recognized as loopback");
        }
    }

    #[tokio::test]
    async fn bind_loopback_refuses_a_non_loopback_address_without_opening_a_socket() {
        let err = bind_loopback("0.0.0.0:0").await.unwrap_err();
        assert!(matches!(err, BindError::NonLoopback { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn bind_loopback_on_an_ephemeral_loopback_port_works() {
        let listener = bind_loopback("127.0.0.1:0").await.expect("127.0.0.1:0 must bind");
        let addr = listener.local_addr().expect("a bound listener has a local address");
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0, "the OS must have assigned a real ephemeral port");
    }
}
