//! End-to-end proof of `av_catalog::client::PgClient` against an in-process fake PostgreSQL
//! server: a `tokio::net::TcpListener` bound to `127.0.0.1:0` (port 0 -- the OS assigns a free
//! port, read back via `local_addr()`, never hard-coded) inside each test, speaking scripted
//! backend messages built with this crate's own `av_catalog::protocol` encoders. This is
//! loopback inside one test process, not "network" in `common.md` question 154's sense -- the
//! same pattern this workspace already uses for its gRPC integration tests
//! (`crates/av-gateway/tests/*.rs`'s own `TestServer`).
//!
//! No Docker, no container: the real PostGIS container proof is a separate, later task (this
//! task's own brief, verbatim). Every test here is a real `PgClient` talking real PostgreSQL
//! v3 wire bytes to a fake server that is, itself, built from nothing but this crate's own
//! `protocol` module -- so a bug in either direction's framing would make these tests fail,
//! not silently pass.

use std::time::{Duration, Instant};

use av_catalog::protocol;
use av_catalog::{CatalogError, Param, PgClient, PgConfig, PgTls};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A self-contained, independent SCRAM-SHA-256 "server side" reference implementation, using
/// the same `openssl` primitives `av_catalog::scram` does but written separately here rather
/// than calling into that module's own (private) helpers -- this task's own brief: "fake
/// server side computes the expected client proof from the RFC primitives". If this file's own
/// computation and `av_catalog::scram`'s disagreed, the handshake test below would fail; it
/// does not, which is the actual end-to-end proof this test module exists to provide.
mod fake_scram_server {
    use openssl::hash::MessageDigest;
    use openssl::pkcs5::pbkdf2_hmac;
    use openssl::pkey::PKey;
    use openssl::sha::sha256;
    use openssl::sign::Signer;

    fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
        let pkey = PKey::hmac(key).expect("PKey::hmac");
        let mut signer = Signer::new(MessageDigest::sha256(), &pkey).expect("Signer::new");
        signer.update(data).expect("Signer::update");
        signer.sign_to_vec().expect("Signer::sign_to_vec").try_into().expect("HMAC-SHA256 is 32 bytes")
    }

    pub struct ServerScram {
        salted_password: [u8; 32],
    }

    impl ServerScram {
        pub fn new(password: &str, salt: &[u8], iterations: u32) -> Self {
            let mut salted_password = [0u8; 32];
            pbkdf2_hmac(password.as_bytes(), salt, iterations as usize, MessageDigest::sha256(), &mut salted_password).expect("pbkdf2_hmac");
            Self { salted_password }
        }

        /// The `ClientProof` a correctly-behaving client (this crate's own `ScramClient`,
        /// given the same password) must have sent.
        pub fn expected_client_proof(&self, auth_message: &str) -> [u8; 32] {
            let client_key = hmac_sha256(&self.salted_password, b"Client Key");
            let stored_key = sha256(&client_key);
            let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
            std::array::from_fn(|i| client_key[i] ^ client_signature[i])
        }

        /// The `ServerSignature` this server sends back in `v=...`.
        pub fn server_signature(&self, auth_message: &str) -> [u8; 32] {
            let server_key = hmac_sha256(&self.salted_password, b"Server Key");
            hmac_sha256(&server_key, auth_message.as_bytes())
        }
    }
}

const PASSWORD: &str = "s3cret-catalog-pw";

fn test_config(port: u16, password: &str) -> PgConfig {
    PgConfig { host: "127.0.0.1".to_string(), port, user: "catalog_app".to_string(), password: password.to_string(), database: "catalog".to_string(), application_name: "av-catalog-wire-protocol-test".to_string(), connect_timeout: Duration::from_secs(5), tls: PgTls::Disabled }
}

async fn read_exact_vec(socket: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    socket.read_exact(&mut buf).await.expect("fake server: read_exact");
    buf
}

/// Reads and decodes a `StartupMessage` (the one frontend message with a 4-byte, not 5-byte,
/// header -- see `av_catalog::protocol`'s own module doc).
async fn read_startup_message(socket: &mut TcpStream) -> protocol::DecodedStartupMessage {
    let header = read_exact_vec(socket, 4).await;
    let body_len = protocol::read_untyped_frame_header(&header.try_into().unwrap()).expect("fake server: StartupMessage header");
    let body = read_exact_vec(socket, body_len).await;
    protocol::decode_startup_message(&body).expect("fake server: StartupMessage body")
}

/// Reads one standard 5-byte-headered frontend message, returning its type byte and body.
async fn read_frontend_message(socket: &mut TcpStream) -> (u8, Vec<u8>) {
    let header = read_exact_vec(socket, 5).await;
    let fh = protocol::read_frame_header(&header.try_into().unwrap()).expect("fake server: frame header");
    let body = read_exact_vec(socket, fh.body_len).await;
    (fh.msg_type, body)
}

/// Plays the fake server's side of a full SCRAM-SHA-256 exchange (module doc's "independent"
/// computation), from immediately after `StartupMessage` through `ReadyForQuery`. Every test
/// below except the ones specifically about a DIFFERENT auth outcome (MD5 refusal, a
/// connection closed mid-message, TLS declined) uses this to get to a ready connection before
/// exercising its own scenario.
async fn fake_server_authenticate(socket: &mut TcpStream, password: &str) {
    let startup = read_startup_message(socket).await;
    assert_eq!(startup.protocol_version, protocol::PROTOCOL_VERSION_3_0);
    assert!(startup.params.contains(&("user".to_string(), "catalog_app".to_string())), "{:?}", startup.params);
    assert!(startup.params.contains(&("client_encoding".to_string(), "UTF8".to_string())), "{:?}", startup.params);

    socket.write_all(&protocol::encode_authentication_sasl(&["SCRAM-SHA-256"])).await.unwrap();

    let (msg_type, body) = read_frontend_message(socket).await;
    assert_eq!(msg_type, b'p', "expected SASLInitialResponse");
    let (mechanism, data) = protocol::decode_sasl_initial_response(&body).expect("fake server: SASLInitialResponse");
    assert_eq!(mechanism, "SCRAM-SHA-256");
    let client_first_message = String::from_utf8(data).expect("client-first-message is UTF-8");
    let client_first_bare = client_first_message.strip_prefix(av_catalog::scram::GS2_HEADER).expect("client-first-message starts with the gs2 header");
    let (_, client_nonce) = client_first_bare.split_once(",r=").expect("client-first-message-bare has an r= nonce");

    let salt = b"0123456789ABCDEF".to_vec();
    let iterations = 4096u32;
    let server_nonce = format!("{client_nonce}fake-server-suffix");
    let server_first_message = format!("r={server_nonce},s={},i={iterations}", BASE64.encode(&salt));
    socket.write_all(&protocol::encode_authentication_sasl_continue(server_first_message.as_bytes())).await.unwrap();

    let (msg_type, body) = read_frontend_message(socket).await;
    assert_eq!(msg_type, b'p', "expected SASLResponse");
    let client_final_message = String::from_utf8(protocol::decode_sasl_response(&body)).expect("client-final-message is UTF-8");
    let (without_proof, proof_b64) = client_final_message.rsplit_once(",p=").expect("client-final-message has a p= proof");
    assert_eq!(without_proof, format!("c=biws,r={server_nonce}"));

    let auth_message = format!("{client_first_bare},{server_first_message},{without_proof}");
    let scram = fake_scram_server::ServerScram::new(password, &salt, iterations);
    let received_proof = BASE64.decode(proof_b64).expect("p= is valid base64");
    assert_eq!(received_proof, scram.expected_client_proof(&auth_message), "fake server: client proof mismatch (wrong password?)");

    let server_final_message = format!("v={}", BASE64.encode(scram.server_signature(&auth_message)));
    socket.write_all(&protocol::encode_authentication_sasl_final(server_final_message.as_bytes())).await.unwrap();
    socket.write_all(&protocol::encode_authentication_ok()).await.unwrap();
    socket.write_all(&protocol::encode_parameter_status("server_version", "17.11")).await.unwrap();
    socket.write_all(&protocol::encode_backend_key_data(4242, 9999)).await.unwrap();
    socket.write_all(&protocol::encode_ready_for_query(protocol::TransactionStatus::Idle)).await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 1. A full SCRAM-SHA-256 handshake, followed by ReadyForQuery.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn full_scram_handshake_reaches_ready_for_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        fake_server_authenticate(&mut socket, PASSWORD).await;
    });

    let config = test_config(port, PASSWORD);
    let client = PgClient::connect(&config).await.expect("connect should succeed: fake server independently verified the real SCRAM client proof");
    drop(client);

    server.await.expect("fake server task panicked (see its own assertions above)");
}

#[tokio::test]
async fn wrong_password_fails_the_handshake_with_a_typed_server_signature_error() {
    // The client authenticates with a DIFFERENT password than the fake server's -- the fake
    // server (which only checks the client's PROOF, not the final server-signature step this
    // crate's own client performs) will still accept the wrong-password proof as a mismatch at
    // its own `assert_eq!` inside `fake_server_authenticate`... no: a wrong CLIENT password
    // means the CLIENT computes a proof the fake server's `expected_client_proof` (computed
    // from the fake server's own, correct, `PASSWORD`) will not match, so the fake server's own
    // assertion fires first. This test instead proves the other direction: the fake server
    // signs with a DIFFERENT (wrong) password than what it told the client to salt against,
    // simulating a compromised/misbehaving server -- `PgClient::connect` must refuse it via the
    // server-signature check ([`CatalogError::ScramServerSignatureMismatch`]), never silently
    // accept.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let startup = read_startup_message(&mut socket).await;
        assert!(startup.params.contains(&("user".to_string(), "catalog_app".to_string())));
        socket.write_all(&protocol::encode_authentication_sasl(&["SCRAM-SHA-256"])).await.unwrap();

        let (_, body) = read_frontend_message(&mut socket).await;
        let (_, data) = protocol::decode_sasl_initial_response(&body).unwrap();
        let client_first_message = String::from_utf8(data).unwrap();
        let client_first_bare = client_first_message.strip_prefix(av_catalog::scram::GS2_HEADER).unwrap();
        let (_, client_nonce) = client_first_bare.split_once(",r=").unwrap();

        let salt = b"0123456789ABCDEF".to_vec();
        let iterations = 4096u32;
        let server_nonce = format!("{client_nonce}fake-server-suffix");
        let server_first_message = format!("r={server_nonce},s={},i={iterations}", BASE64.encode(&salt));
        socket.write_all(&protocol::encode_authentication_sasl_continue(server_first_message.as_bytes())).await.unwrap();

        let (_, body) = read_frontend_message(&mut socket).await;
        let client_final_message = String::from_utf8(protocol::decode_sasl_response(&body)).unwrap();
        let (without_proof, _proof_b64) = client_final_message.rsplit_once(",p=").unwrap();
        let auth_message = format!("{client_first_bare},{server_first_message},{without_proof}");

        // Signs with the WRONG password -- a different SaltedPassword than the client (which
        // correctly used PASSWORD) derived, so the ServerSignature this sends will not match
        // what a correct server would have sent.
        let wrong_scram = fake_scram_server::ServerScram::new("not-the-real-password", &salt, iterations);
        let bogus_server_final = format!("v={}", BASE64.encode(wrong_scram.server_signature(&auth_message)));
        socket.write_all(&protocol::encode_authentication_sasl_final(bogus_server_final.as_bytes())).await.unwrap();
        // The real server would never get this far after signing wrong, but the client should
        // have already refused before reading anything else. Nothing more sent.
    });

    let config = test_config(port, PASSWORD);
    let err = PgClient::connect(&config).await.expect_err("a bad server signature must be refused, never silently accepted");
    assert!(matches!(err, CatalogError::ScramServerSignatureMismatch), "{err:?}");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 2. `query` over the extended protocol: two rows, three columns, including a NULL.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn query_returns_two_rows_three_columns_with_a_null() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        fake_server_authenticate(&mut socket, PASSWORD).await;

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'P');
        let parse = protocol::decode_parse(&body).unwrap();
        assert_eq!(parse.sql, "SELECT id, label, note FROM widgets WHERE id > $1");

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'B');
        let bind = protocol::decode_bind(&body).unwrap();
        assert_eq!(bind.params, vec![Some(b"1".to_vec())]);

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'D');
        protocol::decode_describe(&body).unwrap();

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'E');
        protocol::decode_execute(&body).unwrap();

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'S');
        protocol::decode_sync(&body).unwrap();

        socket.write_all(&protocol::encode_parse_complete()).await.unwrap();
        socket.write_all(&protocol::encode_bind_complete()).await.unwrap();

        let fields = vec![
            protocol::FieldDescription { name: "id".to_string(), table_oid: 16400, column_id: 1, type_oid: 23, type_size: 4, type_modifier: -1, format_code: 0 },
            protocol::FieldDescription { name: "label".to_string(), table_oid: 16400, column_id: 2, type_oid: 25, type_size: -1, type_modifier: -1, format_code: 0 },
            protocol::FieldDescription { name: "note".to_string(), table_oid: 16400, column_id: 3, type_oid: 25, type_size: -1, type_modifier: -1, format_code: 0 },
        ];
        socket.write_all(&protocol::encode_row_description(&fields)).await.unwrap();
        socket.write_all(&protocol::encode_data_row(&[Some(b"2".as_slice()), Some(b"widget-two".as_slice()), None])).await.unwrap();
        socket.write_all(&protocol::encode_data_row(&[Some(b"3".as_slice()), Some(b"widget-three".as_slice()), Some(b"has a note".as_slice())])).await.unwrap();
        socket.write_all(&protocol::encode_command_complete("SELECT 2")).await.unwrap();
        socket.write_all(&protocol::encode_ready_for_query(protocol::TransactionStatus::Idle)).await.unwrap();
    });

    let config = test_config(port, PASSWORD);
    let mut client = PgClient::connect(&config).await.unwrap();
    let rows = client.query("SELECT id, label, note FROM widgets WHERE id > $1", &[Param::I64(1)]).await.unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_i64("id").unwrap(), 2);
    assert_eq!(rows[0].get_str("label").unwrap(), "widget-two");
    assert!(rows[0].is_null("note").unwrap());
    assert!(matches!(rows[0].get_str("note").unwrap_err(), CatalogError::ColumnIsNull { .. }));
    assert_eq!(rows[1].get_i64("id").unwrap(), 3);
    assert_eq!(rows[1].get_str("label").unwrap(), "widget-three");
    assert!(!rows[1].is_null("note").unwrap());
    assert_eq!(rows[1].get_str("note").unwrap(), "has a note");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 3. An ErrorResponse mid-query surfaces as CatalogError::Server with the right SQLSTATE, and
//    the connection is left in a defined state after ReadyForQuery.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn error_response_mid_query_surfaces_as_typed_server_error_then_connection_stays_usable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        fake_server_authenticate(&mut socket, PASSWORD).await;

        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'P');
        protocol::decode_parse(&body).unwrap();
        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'B');
        protocol::decode_bind(&body).unwrap();
        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'D');
        protocol::decode_describe(&body).unwrap();
        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'E');
        protocol::decode_execute(&body).unwrap();
        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'S');
        protocol::decode_sync(&body).unwrap();

        socket.write_all(&protocol::encode_parse_complete()).await.unwrap();
        socket.write_all(&protocol::encode_bind_complete()).await.unwrap();
        let server_error = protocol::ServerError { severity: "ERROR".to_string(), sqlstate: "42703".to_string(), message: "column \"nope\" does not exist".to_string(), ..Default::default() };
        socket.write_all(&protocol::encode_error_response(&server_error)).await.unwrap();
        socket.write_all(&protocol::encode_ready_for_query(protocol::TransactionStatus::Failed)).await.unwrap();

        // Proves the connection is left in a defined state after ReadyForQuery: the fake
        // server expects (and answers) one more, perfectly ordinary command.
        let (msg_type, body) = read_frontend_message(&mut socket).await;
        assert_eq!(msg_type, b'Q');
        assert_eq!(protocol::decode_query(&body).unwrap(), "SELECT 1");
        socket.write_all(&protocol::encode_command_complete("SELECT 1")).await.unwrap();
        socket.write_all(&protocol::encode_ready_for_query(protocol::TransactionStatus::Idle)).await.unwrap();
    });

    let config = test_config(port, PASSWORD);
    let mut client = PgClient::connect(&config).await.unwrap();

    let err = client.query("SELECT * FROM widgets WHERE nope = $1", &[Param::I64(1)]).await.unwrap_err();
    match err {
        CatalogError::Server(server_error) => {
            assert_eq!(server_error.sqlstate, "42703");
            assert_eq!(server_error.message, "column \"nope\" does not exist");
        }
        other => panic!("expected CatalogError::Server, got {other:?}"),
    }

    client.simple_batch("SELECT 1").await.expect("connection must still be usable after a server error, once ReadyForQuery was reached");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 4. AuthenticationMD5Password is refused with the typed "MD5 auth is banned" error.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn md5_auth_is_refused_with_a_typed_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _startup = read_startup_message(&mut socket).await;
        socket.write_all(&protocol::encode_authentication_md5_password([1, 2, 3, 4])).await.unwrap();
        // No further reply expected: a client that correctly refuses MD5 auth sends nothing
        // more (never a PasswordMessage), it simply returns a typed error.
    });

    let config = test_config(port, PASSWORD);
    let err = PgClient::connect(&config).await.expect_err("MD5 auth must be refused, never performed");
    assert!(matches!(err, CatalogError::Md5AuthRefused), "{err:?}");

    server.await.unwrap();
}

#[tokio::test]
async fn cleartext_auth_is_refused_with_a_typed_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _startup = read_startup_message(&mut socket).await;
        socket.write_all(&protocol::encode_authentication_cleartext_password()).await.unwrap();
        // No further reply expected: a client that correctly refuses cleartext auth sends
        // nothing more (never a PasswordMessage), it simply returns a typed error.
    });

    let config = test_config(port, PASSWORD);
    let err = PgClient::connect(&config).await.expect_err("cleartext auth must be refused, never performed");
    assert!(matches!(err, CatalogError::CleartextAuthRefused), "{err:?}");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 5. A connection closed mid-message is a typed error, not a hang and not a panic.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn connection_closed_mid_message_is_a_typed_error_not_a_hang_or_panic() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _startup = read_startup_message(&mut socket).await;
        // Write only the first 3 of what would be a 5-byte AuthenticationSASL frame header,
        // then close -- a connection closed strictly MID-message, not between messages.
        let full = protocol::encode_authentication_sasl(&["SCRAM-SHA-256"]);
        socket.write_all(&full[..3]).await.unwrap();
        drop(socket);
    });

    let config = test_config(port, PASSWORD);
    // A bounded wait, not a production sleep (rule 7 is about production synchronisation, not
    // a test guarding itself against a hang): proves "not a hang", not merely "not a panic".
    let outcome = tokio::time::timeout(Duration::from_secs(10), PgClient::connect(&config)).await.expect("PgClient::connect hung instead of returning after the peer closed mid-message");
    let err = outcome.expect_err("a connection closed mid-message must be a typed error, never a successful connect");
    assert!(matches!(err, CatalogError::ConnectionClosed { .. }), "{err:?}");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 6. PgTls::Required's SSLRequest path: a server that declines ('N') is refused, never
//    silently downgraded to plaintext.
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn tls_required_but_server_declines_ssl_is_a_typed_refusal_not_a_silent_downgrade() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = [0u8; 8];
        socket.read_exact(&mut header).await.unwrap();
        protocol::decode_ssl_request_code(&header[4..8]).expect("fake server: SSLRequest code");
        socket.write_all(b"N").await.unwrap();
    });

    let mut config = test_config(port, PASSWORD);
    config.tls = PgTls::Required { ca_file: None };
    let err = PgClient::connect(&config).await.expect_err("TLS required + server declines must be refused, never a silent plaintext downgrade");
    assert!(matches!(err, CatalogError::ServerDeclinedTls { .. }), "{err:?}");

    server.await.unwrap();
}

// -------------------------------------------------------------------------------------------
// 7. A peer that accepts the TCP connection and then never writes another byte does not hang
//    `PgClient::connect` forever -- each phase after the TCP connect (TLS negotiation, then
//    startup) is bounded by its own `connect_timeout` budget, and the typed error names which
//    phase stalled (question 233 / round 6 defect 5).
// -------------------------------------------------------------------------------------------

/// The per-phase deadline every test below configures. Short enough to keep the suite fast,
/// long enough that ordinary scheduler jitter on a loopback socket does not itself cause a
/// false failure.
const HANDSHAKE_DEADLINE: Duration = Duration::from_millis(50);

/// Upper bound on how long `PgClient::connect` may take to return once `HANDSHAKE_DEADLINE`
/// has elapsed for a stalled phase. Measured locally (debug build, `cargo-slot test -p
/// av-catalog --test wire_protocol -- --nocapture`, five consecutive runs of each test below,
/// against the 50ms `HANDSHAKE_DEADLINE` above): `startup_phase_times_out_...` returned in
/// 52.89ms / 53.80ms / 53.52ms / 53.85ms / 53.46ms, and
/// `tls_negotiation_phase_times_out_...` in 51.90ms / 53.58ms / 53.52ms / 52.46ms / 53.59ms --
/// i.e. within ~4ms of the deadline itself, all scheduler jitter. 10x the deadline (500ms) is a
/// bound that is still tight enough to fail immediately if the timeout wrapper were removed
/// (this task's perturbation: with the wrapper removed, or the phase deadline set to
/// `Duration::MAX`, the call does not return at all within this bound and the outer
/// `tokio::time::timeout` below fires instead).
const UPPER_BOUND: Duration = Duration::from_millis(500);

/// The window each test below asserts the REAL measured `PgClient::connect` duration falls in.
/// `UPPER_BOUND` above cannot serve as that assertion: it is already enforced by the outer
/// `tokio::time::timeout`, so an `elapsed <= UPPER_BOUND` assert after that timeout has
/// returned can never fail -- and an assertion that cannot fail is not a proof (manager review,
/// round 7). These two CAN fail, independently of the outer timeout:
///
/// - the LOWER bound says the call actually waited the deadline out rather than returning
///   instantly. Without it, a connection refused at once, a deadline accidentally set to zero,
///   or any other immediate error that happened to be typed `HandshakeTimeout` would pass the
///   test while proving nothing about a deadline;
/// - the UPPER bound, at 4x the deadline (200ms), is five times tighter than the outer
///   timeout's 500ms, so a phase whose deadline fired late enough to matter fails HERE, with
///   the measured duration in the message, rather than being absorbed by the outer wait.
///
/// The measured spread the worker recorded against this 50ms deadline (five consecutive runs
/// of each test: 51.90ms to 53.85ms, i.e. within ~4ms of the deadline itself) sits comfortably
/// inside this window with about 3.7x of headroom on the upper side.
const MIN_ELAPSED: Duration = HANDSHAKE_DEADLINE;
const MAX_ELAPSED: Duration = Duration::from_millis(200);

#[tokio::test]
async fn startup_phase_times_out_when_peer_accepts_and_never_writes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Accepts the connection and then does nothing at all: no read, no write. `PgClient`'s own
    // `StartupMessage` write still succeeds (it lands in the kernel's receive buffer on this
    // end, unread), but the read that follows -- waiting for `Authentication*` -- never gets a
    // reply. Held alive well past `UPPER_BOUND` so the peer is still "accepted, silent", not
    // "gone", for the whole assertion window; never joined, since it is expected to still be
    // sleeping when the test finishes.
    let _server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let _socket = socket;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut config = test_config(port, PASSWORD);
    config.connect_timeout = HANDSHAKE_DEADLINE;
    let started = Instant::now();
    // A bounded outer wait, not a production sleep (rule 7 is about production synchronisation,
    // not a test guarding itself against its own assertion hanging the suite): this is what
    // actually fails the test (with a clear panic message) if `PgClient::connect` does not
    // return within `UPPER_BOUND` at all, rather than the process hanging silently.
    let outcome = tokio::time::timeout(UPPER_BOUND, PgClient::connect(&config)).await.expect("PgClient::connect did not return within UPPER_BOUND after the peer accepted and never wrote -- the startup-phase deadline did not fire");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MIN_ELAPSED && elapsed <= MAX_ELAPSED,
        "PgClient::connect returned in {elapsed:?}, outside the measured window \
         {MIN_ELAPSED:?}..={MAX_ELAPSED:?} for a {HANDSHAKE_DEADLINE:?} per-phase deadline \
         (below the lower bound means it did not wait the deadline out at all; above the upper \
         bound means the deadline fired late)"
    );

    let err = outcome.expect_err("a peer that accepts and never writes must be a typed handshake timeout, never a successful connect");
    match &err {
        CatalogError::HandshakeTimeout { host, port: err_port, timeout, phase } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(*err_port, port);
            assert_eq!(*timeout, HANDSHAKE_DEADLINE);
            assert_eq!(*phase, "startup");
        }
        other => panic!("expected CatalogError::HandshakeTimeout {{ phase: \"startup\", .. }}, got {other:?}"),
    }
}

#[tokio::test]
async fn tls_negotiation_phase_times_out_when_peer_accepts_and_never_answers_ssl_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Accepts the connection, reads the 8-byte SSLRequest so the client's own write completes
    // normally, then never sends the `'S'`/`'N'` response byte the client is waiting on --
    // exactly "accepts and never writes" for the TLS-negotiation phase specifically. Never
    // joined, for the same reason as the startup-phase test above.
    let _server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = [0u8; 8];
        socket.read_exact(&mut header).await.unwrap();
        protocol::decode_ssl_request_code(&header[4..8]).expect("fake server: SSLRequest code");
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut config = test_config(port, PASSWORD);
    config.tls = PgTls::Required { ca_file: None };
    config.connect_timeout = HANDSHAKE_DEADLINE;
    let started = Instant::now();
    let outcome = tokio::time::timeout(UPPER_BOUND, PgClient::connect(&config)).await.expect("PgClient::connect did not return within UPPER_BOUND after the peer accepted and never answered SSLRequest -- the tls-negotiation-phase deadline did not fire");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= MIN_ELAPSED && elapsed <= MAX_ELAPSED,
        "PgClient::connect returned in {elapsed:?}, outside the measured window \
         {MIN_ELAPSED:?}..={MAX_ELAPSED:?} for a {HANDSHAKE_DEADLINE:?} per-phase deadline \
         (below the lower bound means it did not wait the deadline out at all; above the upper \
         bound means the deadline fired late)"
    );

    let err = outcome.expect_err("a peer that accepts and never answers SSLRequest must be a typed handshake timeout, never a successful connect");
    match &err {
        CatalogError::HandshakeTimeout { host, port: err_port, timeout, phase } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(*err_port, port);
            assert_eq!(*timeout, HANDSHAKE_DEADLINE);
            assert_eq!(*phase, "tls-negotiation");
        }
        other => panic!("expected CatalogError::HandshakeTimeout {{ phase: \"tls-negotiation\", .. }}, got {other:?}"),
    }
}
