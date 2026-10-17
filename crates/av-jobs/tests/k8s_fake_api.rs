//! Question 218 (a fake-server suite per protocol), question 245 heavy-ct-2a: the Kubernetes API
//! client and the Jobs executor against a scripted fake API server speaking TLS on loopback, so the
//! client's error typing, its TLS behaviour and the executor's poll loop are proven without a
//! cluster, and the paths a real cluster cannot be made to take (a second pod, an API server that
//! vanishes mid-run, a refusal at create) are proven at all.
//!
//! The PKI (a CA, a server certificate with chosen SANs, client certificates) is generated at test
//! time with the `openssl` crate. The fake is an `openssl::ssl` server on `127.0.0.1:0` that calls a
//! handler per request and records what it was sent. Nothing here weakens verification: where a test
//! needs "accepted" and "refused" it uses the SAME certificate and different hosts.
//!
//! The executor tests (`run_*`) call the image gate, which asks the local Docker, so they are gated
//! on Docker plus the pinned probe image like every docker-gated test and take `lock_docker_tests`;
//! the client tests need neither.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use av_cdm::pb;
use av_http1::Timeouts;
use av_jobs::k8s::status::K8sError;
use av_jobs::k8s::{ApiError, K8sApi, K8sConfig, K8sJobExecutor, Kubeconfig};
use av_jobs::runner::JobInput;
use av_lockstep::docker::{announce_gate_skip, lock_docker_tests, recorded_digest_gate};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::ec::{EcGroup, EcKey};
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private};
use openssl::ssl::{Ssl, SslContextBuilder, SslMethod, SslVerifyMode, SslVersion};
use openssl::x509::extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName};
use openssl::x509::{X509NameBuilder, X509};
use serde_json::{json, Value};

// ------------------------------------------------------------------------------------------
// PKI.
// ------------------------------------------------------------------------------------------

static SERIAL: AtomicU32 = AtomicU32::new(1);

struct Ca {
    cert: X509,
    key: PKey<Private>,
}

struct Leaf {
    cert: X509,
    key: PKey<Private>,
}

fn new_key() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}

fn builder(cn: &str, key: &PKey<Private>, issuer: Option<&openssl::x509::X509NameRef>) -> openssl::x509::X509Builder {
    let mut b = X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_serial_number(&Asn1Integer::from_bn(&BigNum::from_u32(SERIAL.fetch_add(1, Ordering::Relaxed)).unwrap()).unwrap()).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", cn).unwrap();
    let name = name.build();
    b.set_subject_name(&name).unwrap();
    match issuer {
        Some(i) => b.set_issuer_name(i).unwrap(),
        None => b.set_issuer_name(&name).unwrap(),
    }
    b.set_pubkey(key).unwrap();
    b.set_not_before(&Asn1Time::days_from_now(0).unwrap()).unwrap();
    b.set_not_after(&Asn1Time::days_from_now(2).unwrap()).unwrap();
    b
}

fn make_ca(cn: &str) -> Ca {
    let key = new_key();
    let mut b = builder(cn, &key, None);
    b.append_extension(BasicConstraints::new().critical().ca().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().key_cert_sign().crl_sign().build().unwrap()).unwrap();
    b.sign(&key, openssl::hash::MessageDigest::sha256()).unwrap();
    Ca { cert: b.build(), key }
}

fn issue(ca: &Ca, cn: &str, dns: &[&str], ips: &[&str], client: bool) -> Leaf {
    let key = new_key();
    let mut b = builder(cn, &key, Some(ca.cert.subject_name()));
    b.append_extension(BasicConstraints::new().build().unwrap()).unwrap();
    b.append_extension(KeyUsage::new().critical().digital_signature().build().unwrap()).unwrap();
    let mut eku = ExtendedKeyUsage::new();
    if client {
        eku.client_auth();
    } else {
        eku.server_auth();
    }
    b.append_extension(eku.build().unwrap()).unwrap();
    if !dns.is_empty() || !ips.is_empty() {
        let mut san = SubjectAlternativeName::new();
        for d in dns {
            san.dns(d);
        }
        for i in ips {
            san.ip(i);
        }
        let ext = san.build(&b.x509v3_context(Some(&ca.cert), None)).unwrap();
        b.append_extension(ext).unwrap();
    }
    b.sign(&ca.key, openssl::hash::MessageDigest::sha256()).unwrap();
    Leaf { cert: b.build(), key }
}

fn b64(bytes: &[u8]) -> String {
    openssl::base64::encode_block(bytes)
}

fn kubeconfig_for(server: &str, ca: &Ca, client: &Leaf) -> Kubeconfig {
    let yaml = format!(
        "current-context: t\nclusters:\n- name: c\n  cluster:\n    server: {server}\n    certificate-authority-data: {}\n\
         contexts:\n- name: t\n  context: {{cluster: c, user: u}}\nusers:\n- name: u\n  user:\n    client-certificate-data: {}\n    client-key-data: {}\n",
        b64(&ca.cert.to_pem().unwrap()),
        b64(&client.cert.to_pem().unwrap()),
        b64(&client.key.private_key_to_pem_pkcs8().unwrap())
    );
    Kubeconfig::parse(&yaml, None, None).unwrap()
}

// ------------------------------------------------------------------------------------------
// The fake API server.
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

enum Reply {
    Json(u16, Value),
    Text(u16, &'static str, String),
    /// Close the connection without answering.
    Drop,
}

fn status(code: u16, reason: &str, message: &str) -> Reply {
    Reply::Json(code, json!({"kind": "Status", "apiVersion": "v1", "metadata": {}, "status": "Failure", "message": message, "reason": reason, "code": code}))
}

type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;

struct Fake {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Req>>>,
    handshake_failures: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

impl Fake {
    fn port(&self) -> u16 {
        self.addr.port()
    }
    fn requests(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn read_request<S: Read>(s: &mut S) -> Option<Req> {
    let mut got = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    };
    let head = String::from_utf8_lossy(&got[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    while got.len() < head_end + len {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&tmp[..n]),
        }
    }
    Some(Req { method, path, headers, body: got[head_end..].to_vec() })
}

/// A TLS fake. `require_client_ca`: demand a client certificate issued by that CA.
fn serve(leaf: &Leaf, require_client_ca: Option<&Ca>, handler: Handler) -> Fake {
    let mut ctx = SslContextBuilder::new(SslMethod::tls_server()).unwrap();
    ctx.set_min_proto_version(Some(SslVersion::TLS1_2)).unwrap();
    ctx.set_certificate(&leaf.cert).unwrap();
    ctx.set_private_key(&leaf.key).unwrap();
    if let Some(ca) = require_client_ca {
        ctx.cert_store_mut().add_cert(ca.cert.clone()).unwrap();
        ctx.set_verify(SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT);
    }
    let ctx = ctx.build();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let failures = Arc::new(AtomicUsize::new(0));
    let (stop2, requests2, failures2) = (stop.clone(), requests.clone(), failures.clone());
    let handle = thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((tcp, _)) => {
                    tcp.set_nonblocking(false).ok();
                    tcp.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    tcp.set_write_timeout(Some(Duration::from_secs(5))).ok();
                    let Ok(mut stream) = Ssl::new(&ctx).unwrap().accept(tcp) else {
                        failures2.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    let Some(req) = read_request(&mut stream) else { continue };
                    requests2.lock().unwrap().push(req.clone());
                    let (code, ctype, body) = match handler(&req) {
                        Reply::Json(code, v) => (code, "application/json", v.to_string()),
                        Reply::Text(code, ctype, body) => (code, ctype, body),
                        Reply::Drop => continue,
                    };
                    let head = format!("HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                    let _ = stream.flush();
                    let _ = stream.shutdown();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
    });
    Fake { addr, stop, requests, handshake_failures: failures, handle: Some(handle) }
}

fn api_for(cfg: &Kubeconfig) -> K8sApi {
    K8sApi::new(cfg, Timeouts::uniform(Duration::from_secs(3))).unwrap()
}

fn node(name: &str, ready: bool, runtime: &str) -> Value {
    json!({"metadata": {"name": name}, "status": {"conditions": [{"type": "Ready", "status": if ready {"True"} else {"False"}}], "nodeInfo": {"containerRuntimeVersion": runtime}}})
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/k8s/{name}.json"));
    serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))).unwrap()
}

// ------------------------------------------------------------------------------------------
// The client: error typing, TLS, identity.
// ------------------------------------------------------------------------------------------

#[test]
fn error_replies_are_typed_from_the_kubernetes_status_over_tls() {
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/apis/batch/v1/namespaces/ns/jobs/forbidden") => status(403, "Forbidden", "jobs.batch \"forbidden\" is forbidden: User \"av\" cannot get resource \"jobs\""),
            ("GET", "/apis/batch/v1/namespaces/ns/jobs/missing") => status(404, "NotFound", "jobs.batch \"missing\" not found"),
            ("POST", "/apis/batch/v1/namespaces/ns/jobs") => status(409, "AlreadyExists", "jobs.batch \"dup\" already exists"),
            ("GET", "/apis/batch/v1/namespaces/ns/jobs/proxy") => Reply::Text(502, "text/html", "<html>upstream gone</html>".to_string()),
            ("GET", "/apis/batch/v1/namespaces/ns/jobs/garbage") => Reply::Text(200, "text/plain", "this is not json".to_string()),
            ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": [node("n1", true, "docker://29")]})),
            ("GET", p) if p.starts_with("/api/v1/namespaces/ns/pods/p1/log") => Reply::Text(200, "text/plain", "line one\nline two\n".to_string()),
            _ => status(500, "InternalError", "unscripted"),
        }),
    );
    let api = api_for(&kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client));

    match api.get_job("ns", "forbidden") {
        Err(ApiError::Refused(s)) => {
            assert_eq!((s.code, s.reason.as_str()), (403, "Forbidden"));
            assert!(s.message.contains("cannot get resource"), "{s:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(api.get_job("ns", "missing").unwrap_err().is_not_found());
    let manifest = json!({"kind": "Job", "metadata": {"name": "dup"}});
    let err = api.create_job("ns", &manifest).unwrap_err();
    assert!(err.is_conflict(), "{err:?}");
    match api.get_job("ns", "proxy") {
        Err(ApiError::Refused(s)) => {
            assert_eq!((s.code, s.reason.as_str()), (502, ""));
            assert!(s.message.contains("upstream gone"), "{s:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(api.get_job("ns", "garbage"), Err(ApiError::BadBody { what: "get job", .. })));
    assert_eq!(api.list_nodes().unwrap().len(), 1);
    assert_eq!(api.pod_log("ns", "p1", "job", 10, 1000).unwrap(), b"line one\nline two\n");

    // What went over the wire: the JSON body and headers of the create, the encoded log query.
    let reqs = fake.requests();
    let create = reqs.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(create.json(), manifest);
    assert_eq!(create.header("Content-Type"), Some("application/json"));
    assert_eq!(create.header("Accept"), Some("application/json"));
    let log = reqs.iter().find(|r| r.path.contains("/log")).unwrap();
    assert_eq!(log.path, "/api/v1/namespaces/ns/pods/p1/log?container=job&tailLines=10&limitBytes=1000");
}

#[test]
fn the_client_certificate_is_required_and_a_foreign_one_is_refused() {
    let ca = make_ca("fake CA");
    let other_ca = make_ca("some other CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let good_client = issue(&ca, "av-jobs", &[], &[], true);
    let foreign_client = issue(&other_ca, "intruder", &[], &[], true);
    let fake = serve(&server, Some(&ca), Arc::new(|_| Reply::Json(200, json!({"items": []}))));
    let url = format!("https://127.0.0.1:{}", fake.port());

    assert!(api_for(&kubeconfig_for(&url, &ca, &good_client)).list_nodes().is_ok());
    let refused = api_for(&kubeconfig_for(&url, &ca, &foreign_client)).list_nodes();
    assert!(matches!(refused, Err(ApiError::Tls(_)) | Err(ApiError::Unreachable(_))), "a certificate from another CA must not be accepted: {refused:?}");
    assert!(fake.handshake_failures.load(Ordering::Relaxed) >= 1, "the server must have rejected the handshake");
    assert_eq!(fake.requests().len(), 1, "only the good client got as far as a request");
}

#[test]
fn the_same_server_certificate_is_accepted_by_ip_and_refused_by_the_wrong_name() {
    let ca = make_ca("fake CA");
    // Valid for 127.0.0.1 only: no DNS SAN for `localhost`, and the CN is no fallback.
    let server = issue(&ca, "localhost", &[], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let fake = serve(&server, None, Arc::new(|_| Reply::Json(200, json!({"items": []}))));

    let by_ip = api_for(&kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client));
    assert!(by_ip.list_nodes().is_ok(), "accepted by the host the certificate names");

    let by_name = api_for(&kubeconfig_for(&format!("https://localhost:{}", fake.port()), &ca, &client));
    match by_name.list_nodes() {
        Err(ApiError::Tls(reason)) => assert!(reason.to_ascii_lowercase().contains("verif") || reason.contains("certificate"), "{reason}"),
        other => panic!("the same certificate presented under a name it does not carry must be refused: {other:?}"),
    }
}

#[test]
fn a_server_signed_by_an_unknown_ca_is_refused() {
    let trusted = make_ca("trusted CA");
    let rogue = make_ca("rogue CA");
    let server = issue(&rogue, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&trusted, "av-jobs", &[], &[], true);
    let fake = serve(&server, None, Arc::new(|_| Reply::Json(200, json!({"items": []}))));
    let api = api_for(&kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &trusted, &client));
    assert!(matches!(api.list_nodes(), Err(ApiError::Tls(_))));
    assert!(fake.requests().is_empty(), "no request may be sent over an unverified channel");
}

// ------------------------------------------------------------------------------------------
// The executor's start-up checks, against the fake.
// ------------------------------------------------------------------------------------------

fn fake_cluster(nodes: Vec<Value>, namespace_exists: bool) -> (Fake, Kubeconfig) {
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(move |r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": nodes})),
            ("GET", "/api/v1/namespaces/ns") if namespace_exists => Reply::Json(200, json!({"metadata": {"name": "ns"}})),
            ("GET", "/api/v1/namespaces/ns") => status(404, "NotFound", "namespaces \"ns\" not found"),
            _ if r.path.contains("/networkpolicies") => netpol_absent(r),
            _ => status(500, "InternalError", "unscripted"),
        }),
    );
    let cfg = kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client);
    (fake, cfg)
}

fn fake_scratch_root() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.av-test-tmp/av-jobs-k8s-fake-test");
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_cluster_with_two_nodes_and_none_named_is_refused_and_naming_one_fixes_it() {
    let (fake, cfg) = fake_cluster(vec![node("a", true, "docker://29"), node("b", true, "docker://29")], true);
    match K8sJobExecutor::new(accepting_window(cfg.clone())) {
        Err(K8sError::Preflight(m)) => assert!(m.contains("2 nodes") && m.contains("must be named"), "{m}"),
        other => panic!("{other:?}"),
    }
    let mut named = accepting_window(cfg);
    named.node_name = Some("b".to_string());
    assert_eq!(K8sJobExecutor::new(named).unwrap().node_name(), "b");
    drop(fake);
}

#[test]
fn start_up_refuses_a_missing_namespace_an_unready_node_a_non_docker_node_and_an_unreachable_server() {
    let (_f1, cfg) = fake_cluster(vec![node("a", true, "docker://29")], false);
    assert!(matches!(K8sJobExecutor::new(accepting_window(cfg)), Err(K8sError::Preflight(m)) if m.contains("namespace \"ns\" does not exist")));
    let (_f2, cfg) = fake_cluster(vec![node("a", false, "docker://29")], true);
    assert!(matches!(K8sJobExecutor::new(accepting_window(cfg)), Err(K8sError::Preflight(m)) if m.contains("not Ready")));
    let (_f3, cfg) = fake_cluster(vec![node("a", true, "containerd://2.0")], true);
    assert!(matches!(K8sJobExecutor::new(accepting_window(cfg)), Err(K8sError::Preflight(m)) if m.contains("not Docker")));

    // A server that is not there: connection refused is typed unreachable, not a panic.
    let (f4, cfg) = fake_cluster(vec![node("a", true, "docker://29")], true);
    drop(f4);
    let mut c = accepting_window(cfg);
    c.timeouts = Timeouts::uniform(Duration::from_secs(1));
    assert!(matches!(K8sJobExecutor::new(c), Err(K8sError::ApiUnreachable { op: "list nodes", .. })));
}

// ------------------------------------------------------------------------------------------
// The executor's whole run, against the fake (docker-gated: the image gate asks Docker).
// ------------------------------------------------------------------------------------------

const CONTAINER_IMAGE: &str = "alpine";

/// `(image digest recorded for the probe image)` or `None` after a visible SKIPPED.
fn docker_gate(test_name: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/CONTAINER_EXECUTOR_IMAGE_DIGEST.md");
    let text = fs::read_to_string(&path).unwrap();
    let block = |marker: &str| -> String {
        let lines: Vec<&str> = text.lines().collect();
        let m = lines.iter().position(|l| l.contains(marker)).unwrap();
        let s = m + lines[m..].iter().position(|l| l.trim() == "```").unwrap();
        let e = s + 1 + lines[s + 1..].iter().position(|l| l.trim() == "```").unwrap();
        lines[s + 1..e].join("\n").trim().to_string()
    };
    let (image_ref, id) = (block("Registry reference"), block("docker image inspect"));
    match recorded_digest_gate(&image_ref, &id) {
        Ok(()) => Some(id),
        Err(reason) => {
            announce_gate_skip(test_name, &reason);
            None
        }
    }
}

fn spec(job_id: &str, digest: &str) -> pb::JobSpec {
    pb::JobSpec {
        job_id: job_id.to_string(),
        kind: "k8s-fake".to_string(),
        label: Some(pb::Label { marking: "CUI".to_string(), caveats: vec![] }),
        requested_tai_ns: 1,
        executor: pb::JobExecutorKind::Container as i32,
        command: vec!["/bin/true".to_string()],
        container_image: CONTAINER_IMAGE.to_string(),
        container_image_digest: digest.to_string(),
        ..Default::default()
    }
}

fn quick_config(cfg: Kubeconfig) -> K8sConfig {
    let mut c = accepting_window(cfg);
    c.timeouts = Timeouts::uniform(Duration::from_secs(3));
    c.poll_interval = Duration::from_millis(20);
    c.deadline = Duration::from_secs(20);
    c.margin = Duration::from_secs(5);
    c
}

/// Shared state of a scripted job: the number of pod listings so far and whether the Job was deleted.
#[derive(Default)]
struct Script {
    pod_lists: AtomicUsize,
    deleted: AtomicBool,
}

/// A fake that understands the executor's whole conversation. `jobs_pods(n)` gives the Job status and
/// pods the n-th poll (0-based) sees; `on_create` runs when the Job manifest arrives.
fn scripted(
    nodes: Vec<Value>,
    script: Arc<Script>,
    on_create: impl Fn(&Value) + Send + Sync + 'static,
    jobs_pods: impl Fn(usize) -> (Value, Vec<Value>) + Send + Sync + 'static,
    log: &'static str,
) -> (Fake, Kubeconfig) {
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(move |r: &Req| {
            let path = r.path.split('?').next().unwrap_or("");
            match (r.method.as_str(), path) {
                ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": nodes})),
                ("GET", "/api/v1/namespaces/ns") => Reply::Json(200, json!({"metadata": {"name": "ns"}})),
                ("GET", "/api/v1/namespaces/default/services/kubernetes") => Reply::Json(200, json!({"spec": {"clusterIP": "10.43.0.7", "ports": [{"port": 6443}]}})),
                ("POST", "/apis/batch/v1/namespaces/ns/jobs") => {
                    let manifest = r.json();
                    on_create(&manifest);
                    Reply::Json(201, manifest)
                }
                ("GET", p) if p.starts_with("/apis/batch/v1/namespaces/ns/jobs/") => {
                    if script.deleted.load(Ordering::Relaxed) {
                        return status(404, "NotFound", "jobs.batch not found");
                    }
                    let n = script.pod_lists.load(Ordering::Relaxed);
                    let (mut job, _) = jobs_pods(n);
                    job["metadata"] = json!({"name": p.rsplit('/').next().unwrap()});
                    Reply::Json(200, job)
                }
                ("GET", "/api/v1/namespaces/ns/pods") => {
                    let n = script.pod_lists.fetch_add(1, Ordering::Relaxed);
                    Reply::Json(200, json!({"items": jobs_pods(n).1}))
                }
                ("GET", p) if p.ends_with("/log") => Reply::Text(200, "text/plain", log.to_string()),
                ("DELETE", p) if p.starts_with("/apis/batch/v1/namespaces/ns/jobs/") => {
                    script.deleted.store(true, Ordering::Relaxed);
                    Reply::Json(200, json!({"kind": "Status", "status": "Success"}))
                }
                _ if r.path.contains("/networkpolicies") => netpol_absent(r),
                _ => status(500, "InternalError", "unscripted"),
            }
        }),
    );
    let cfg = kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client);
    (fake, cfg)
}

fn host_path(manifest: &Value, volume: &str) -> PathBuf {
    let vols = manifest["spec"]["template"]["spec"]["volumes"].as_array().unwrap();
    let v = vols.iter().find(|v| v["name"] == volume).unwrap();
    PathBuf::from(v["hostPath"]["path"].as_str().unwrap())
}

fn named_pod(fixture_name: &str, name: &str) -> Value {
    let mut pod = fixture(fixture_name);
    pod["metadata"]["name"] = json!(name);
    pod
}

#[test]
fn run_end_to_end_over_the_fake_stages_inputs_collects_outputs_and_tears_down() {
    let _lock = lock_docker_tests();
    let Some(digest) = docker_gate("run_end_to_end_over_the_fake_stages_inputs_collects_outputs_and_tears_down") else { return };
    let script = Arc::new(Script::default());
    /// What the scripted "pod" saw when the Job arrived: (in dir, out dir, input bytes, out dir mode).
    type Staged = (PathBuf, PathBuf, Vec<u8>, u32);
    let staged: Arc<Mutex<Option<Staged>>> = Arc::new(Mutex::new(None));
    let staged2 = staged.clone();
    let (fake, cfg) = scripted(
        vec![node("colima", true, "docker://29")],
        script.clone(),
        move |manifest| {
            // Play the pod: read the staged input, write an output, as a pod with these mounts would.
            let (in_dir, out_dir) = (host_path(manifest, "in"), host_path(manifest, "out"));
            let input = fs::read(in_dir.join("000")).unwrap();
            let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&out_dir).unwrap().permissions()) & 0o777;
            let mut output = input.clone();
            output.make_ascii_uppercase();
            fs::write(out_dir.join("result"), output).unwrap();
            *staged2.lock().unwrap() = Some((in_dir, out_dir, input, mode));
        },
        |n| match n {
            0 => (json!({"status": {"active": 1}}), vec![named_pod("success.pod", "pod-1")]),
            _ => {
                let job = fixture("success.job");
                (json!({"status": job["status"]}), vec![named_pod("success.pod", "pod-1")])
            }
        },
        "hello from the pod\n",
    );
    let mut config = quick_config(cfg);
    config.network_gate = Some(av_jobs::k8s::NetworkGate { image: CONTAINER_IMAGE.to_string(), image_digest: digest.clone(), within: Duration::from_secs(9) });
    let executor = K8sJobExecutor::new(config).unwrap();
    let inputs = vec![JobInput { asset: pb::AssetRef::default(), bytes: b"fake input".to_vec() }];
    let report = executor.run(&spec("fake-e2e", &digest), &inputs);

    let outputs = report.result.as_ref().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].bytes, b"FAKE INPUT");
    assert_eq!(report.log.as_ref().unwrap().text, "hello from the pod\n");
    assert!(report.cleanup.is_ok(), "{:?}", report.cleanup);
    let (in_dir, out_dir, input, out_mode) = staged.lock().unwrap().clone().unwrap();
    assert_eq!(input, b"fake input", "the input was staged as in/000");
    assert_eq!(out_mode, 0o777, "the output directory is world-writable for the non-root pod");
    assert!(!in_dir.exists() && !out_dir.exists(), "scratch directory removed");

    // The conversation, in order: Job created, polled, log read, Job deleted with Foreground propagation, then gone.
    let reqs = fake.requests();
    let seq: Vec<String> = reqs.iter().map(|r| format!("{} {}", r.method, r.path.split('?').next().unwrap())).collect();
    let pos = |needle: &str| seq.iter().position(|s| s.contains(needle)).unwrap_or_else(|| panic!("{needle} not in {seq:?}"));
    assert!(pos("POST /apis/batch/v1/namespaces/ns/jobs") < pos("/pods") && pos("/pods") < pos("/log") && pos("/log") < pos("DELETE"), "{seq:?}");
    let delete = reqs.iter().find(|r| r.method == "DELETE").unwrap();
    assert_eq!(delete.json()["propagationPolicy"], "Foreground");
    let create = reqs.iter().find(|r| r.method == "POST" && r.path.ends_with("/jobs")).unwrap().json();
    assert_eq!(create["spec"]["backoffLimit"], 0);
    // The gate probes the cluster's own API service address, read from default/kubernetes.
    let gate = &create["spec"]["template"]["spec"]["initContainers"][0];
    assert_eq!(gate["name"], "net-gate");
    assert_eq!(gate["image"], format!("alpine@{digest}"));
    let script = gate["command"][2].as_str().unwrap();
    assert!(script.contains("nc -z -w 1 10.43.0.7 6443 || exit 0") && script.contains("+ 9 )"), "{script}");
    let policy_posts = reqs.iter().filter(|r| r.method == "POST" && r.path.ends_with("/networkpolicies")).count();
    assert_eq!(policy_posts, 1, "the default-deny policy is ensured once at start-up");
    assert_eq!(create["spec"]["template"]["spec"]["nodeName"], "colima", "pinned to the cluster's one node");
    assert_eq!(create["spec"]["template"]["spec"]["containers"][0]["image"], format!("alpine@{digest}"));
    let log_req = reqs.iter().find(|r| r.path.contains("/log")).unwrap();
    assert!(log_req.path.contains("container=job") && log_req.path.contains("tailLines=1001") && log_req.path.contains("limitBytes=65537"), "{}", log_req.path);
}

#[test]
fn run_refuses_a_second_pod_whatever_it_did_and_waits_for_the_job_before_deciding() {
    let _lock = lock_docker_tests();
    let Some(digest) = docker_gate("run_refuses_a_second_pod_whatever_it_did_and_waits_for_the_job_before_deciding") else { return };
    let script = Arc::new(Script::default());
    let (_fake, cfg) = scripted(
        vec![node("colima", true, "docker://29")],
        script,
        |_| {},
        |n| {
            // Poll 0: running. Poll 1: the first pod is killed but the Job is not Failed yet (a
            // backoffLimit above 0): nothing may be decided. Poll 2: the replacement exists.
            let first_running = named_pod("docker_kill.pod", "pod-1");
            let mut running = first_running.clone();
            running["status"] = json!({"phase": "Running", "containerStatuses": [{"name": "job", "state": {"running": {}}}]});
            let second = named_pod("docker_kill.pod", "pod-2");
            let mut second_running = second.clone();
            second_running["status"] = running["status"].clone();
            match n {
                0 => (json!({"status": {"active": 1}}), vec![running]),
                1 => (json!({"status": {"failed": 1}}), vec![first_running]),
                _ => (json!({"status": {"failed": 1, "active": 1}}), vec![first_running, second_running]),
            }
        },
        "boom\n",
    );
    let executor = K8sJobExecutor::new(quick_config(cfg)).unwrap();
    let report = executor.run(&spec("fake-retry", &digest), &[]);
    let failure = report.result.as_ref().err().map(|e| e.to_failure(&report.job_id, &report.pods, None)).expect("a retry is a failure");
    println!("{failure:?}");
    assert!(matches!(report.result, Err(K8sError::Retried { ref pods }) if pods == &vec!["pod-1".to_string(), "pod-2".to_string()]), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    assert!(failure.detail.contains("more than one pod"), "{}", failure.detail);
    assert_eq!(report.pods, vec!["pod-1".to_string(), "pod-2".to_string()]);
    assert!(report.cleanup.is_ok());
}

#[test]
fn run_types_a_refusal_at_create_and_leaves_nothing_behind() {
    let _lock = lock_docker_tests();
    let Some(digest) = docker_gate("run_types_a_refusal_at_create_and_leaves_nothing_behind") else { return };
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": [node("colima", true, "docker://29")]})),
            ("GET", "/api/v1/namespaces/ns") => Reply::Json(200, json!({})),
            ("POST", "/apis/batch/v1/namespaces/ns/jobs") => status(403, "Forbidden", "jobs.batch is forbidden: User \"av\" cannot create resource \"jobs\" in namespace \"ns\""),
            _ if r.path.contains("/networkpolicies") => netpol_absent(r),
                _ => status(500, "InternalError", "unscripted"),
        }),
    );
    let cfg = kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client);
    let executor = K8sJobExecutor::new(quick_config(cfg)).unwrap();
    let report = executor.run(&spec("fake-forbidden", &digest), &[]);
    let failure = report.result.as_ref().err().map(|e| e.to_failure(&report.job_id, &report.pods, None)).unwrap();
    println!("{failure:?}");
    assert!(matches!(&report.result, Err(K8sError::ApiRefused { op: "create job", status }) if status.code == 403 && status.reason == "Forbidden"), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    assert!(failure.detail.contains("403") && failure.detail.contains("Forbidden") && failure.detail.contains("cannot create resource"), "{}", failure.detail);
    assert!(report.pods.is_empty());
    assert!(report.cleanup.is_ok());
    assert!(!fake.requests().iter().any(|r| r.method == "DELETE"), "a refused create left no Job of ours to delete");
    assert!(!fake_scratch_root().join(&report.job_name).exists());
}

#[test]
fn run_fails_unreachable_after_the_poll_error_budget_and_still_removes_the_scratch_directory() {
    let _lock = lock_docker_tests();
    let Some(digest) = docker_gate("run_fails_unreachable_after_the_poll_error_budget_and_still_removes_the_scratch_directory") else { return };
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let polls = Arc::new(AtomicUsize::new(0));
    let polls2 = polls.clone();
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(move |r: &Req| match (r.method.as_str(), r.path.split('?').next().unwrap()) {
            ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": [node("colima", true, "docker://29")]})),
            ("GET", "/api/v1/namespaces/ns") => Reply::Json(200, json!({})),
            ("POST", "/apis/batch/v1/namespaces/ns/jobs") => Reply::Json(201, r.json()),
            _ if r.path.contains("/networkpolicies") => netpol_absent(r),
            // From here on the API server is gone: it accepts the connection and hangs up.
            _ => {
                polls2.fetch_add(1, Ordering::Relaxed);
                Reply::Drop
            }
        }),
    );
    let cfg = kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client);
    let mut c = quick_config(cfg);
    c.poll_error_budget = 2;
    let executor = K8sJobExecutor::new(c).unwrap();
    let report = executor.run(&spec("fake-gone", &digest), &[]);
    let failure = report.result.as_ref().err().map(|e| e.to_failure(&report.job_id, &report.pods, None)).unwrap();
    println!("{failure:?}\ncleanup: {:?}", report.cleanup);
    assert!(matches!(report.result, Err(K8sError::ApiUnreachable { op: "get job", .. })), "{:?}", report.result);
    assert_eq!(failure.kind, pb::JobFailureKind::ExecutorUnavailable as i32);
    assert!(failure.detail.contains("unreachable"), "{}", failure.detail);
    // Budget 2 means the third consecutive failure ends the job (the polls before it were retried,
    // not the job).
    assert!(polls.load(Ordering::Relaxed) >= 3, "polls: {}", polls.load(Ordering::Relaxed));
    // The Job could not be deleted either (the server is gone): that is reported, not hidden, ...
    assert!(report.cleanup.is_err(), "{:?}", report.cleanup);
    // ... but the scratch directory is removed regardless.
    assert!(!fake_scratch_root().join(&report.job_name).exists());
}

// ------------------------------------------------------------------------------------------
// The default-deny NetworkPolicy and the network gate, against the fake.
// ------------------------------------------------------------------------------------------

/// A fake namespace with no policy yet: GET is 404, POST is accepted and echoed.
fn netpol_absent(r: &Req) -> Reply {
    match r.method.as_str() {
        "GET" => status(404, "NotFound", "networkpolicies.networking.k8s.io \"av-job-default-deny\" not found"),
        "POST" => Reply::Json(201, r.json()),
        _ => status(405, "MethodNotAllowed", "unscripted"),
    }
}

/// A fake whose namespace already has `existing` as the policy, or (`None`) none at first, and
/// `race`: the first POST answers 409 and from then on GET returns `winner`.
fn netpol_cluster(existing: Option<Value>, race_winner: Option<Value>) -> (Fake, Kubeconfig, Arc<Mutex<Vec<String>>>) {
    let ca = make_ca("fake CA");
    let server = issue(&ca, "fake api", &["localhost"], &["127.0.0.1"], false);
    let client = issue(&ca, "av-jobs", &[], &[], true);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls2 = calls.clone();
    let created = Arc::new(AtomicBool::new(false));
    let fake = serve(
        &server,
        Some(&ca),
        Arc::new(move |r: &Req| {
            if r.path.contains("/networkpolicies") {
                calls2.lock().unwrap().push(r.method.clone());
            }
            match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/api/v1/nodes") => Reply::Json(200, json!({"items": [node("colima", true, "docker://29")]})),
                ("GET", "/api/v1/namespaces/ns") => Reply::Json(200, json!({})),
                ("GET", p) if p.contains("/networkpolicies/") => match (&existing, &race_winner, created.load(Ordering::Relaxed)) {
                    (Some(e), _, _) => Reply::Json(200, e.clone()),
                    (None, Some(w), true) => Reply::Json(200, w.clone()),
                    _ => status(404, "NotFound", "not found"),
                },
                ("POST", p) if p.contains("/networkpolicies") => {
                    if race_winner.is_some() {
                        created.store(true, Ordering::Relaxed);
                        status(409, "AlreadyExists", "networkpolicies \"av-job-default-deny\" already exists")
                    } else {
                        Reply::Json(201, r.json())
                    }
                }
                _ => status(500, "InternalError", "unscripted"),
            }
        }),
    );
    let cfg = kubeconfig_for(&format!("https://127.0.0.1:{}", fake.port()), &ca, &client);
    (fake, cfg, calls)
}

fn deny_policy() -> Value {
    av_jobs::k8s::manifest::network_policy("ns")
}

#[test]
fn start_up_creates_the_default_deny_policy_when_absent_and_leaves_a_matching_one_alone() {
    let (fake, cfg, calls) = netpol_cluster(None, None);
    K8sJobExecutor::new(accepting_window(cfg)).unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["GET", "POST"]);
    let post = fake.requests().into_iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(post.path, "/apis/networking.k8s.io/v1/namespaces/ns/networkpolicies");
    assert_eq!(post.json(), deny_policy());

    let (_f2, cfg, calls) = netpol_cluster(Some(deny_policy()), None);
    K8sJobExecutor::new(accepting_window(cfg)).unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["GET"], "an existing matching policy is neither rewritten nor recreated");
}

#[test]
fn start_up_refuses_a_same_named_policy_that_is_not_default_deny() {
    for (what, mut spec) in [
        ("an allow-all egress rule", json!({"podSelector": {"matchLabels": {"av.job": "1"}}, "policyTypes": ["Ingress", "Egress"], "egress": [{}]})),
        ("egress only", json!({"podSelector": {"matchLabels": {"av.job": "1"}}, "policyTypes": ["Egress"]})),
        ("a different selector", json!({"podSelector": {}, "policyTypes": ["Ingress", "Egress"]})),
    ] {
        let mut policy = deny_policy();
        policy["spec"] = std::mem::take(&mut spec);
        let (_f, cfg, calls) = netpol_cluster(Some(policy), None);
        match K8sJobExecutor::new(accepting_window(cfg)) {
            Err(K8sError::Preflight(m)) => assert!(m.contains("av-job-default-deny") && m.contains("not the default-deny"), "{what}: {m}"),
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(*calls.lock().unwrap(), vec!["GET"], "{what}: a differing policy is never overwritten");
    }
}

#[test]
fn two_executors_starting_at_once_resolve_the_create_race_by_reading_the_winner() {
    let (_f, cfg, calls) = netpol_cluster(None, Some(deny_policy()));
    K8sJobExecutor::new(accepting_window(cfg.clone())).unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["GET", "POST", "GET"]);
    // And a loser who finds a different policy where it expected the winner's still refuses.
    let mut other = deny_policy();
    other["spec"]["egress"] = json!([{}]);
    let (_f2, cfg2, _) = netpol_cluster(None, Some(other));
    assert!(matches!(K8sJobExecutor::new(accepting_window(cfg2)), Err(K8sError::Preflight(_))));
}

// ------------------------------------------------------------------------------------------
// Fail closed on the network: the explicit choice every config makes.
// ------------------------------------------------------------------------------------------

/// The tests above exercise the client, the start-up checks and the poll loop, not the gate; each
/// makes the explicit choice to accept the start-up network window (the gate has its own tests).
fn accepting_window(cfg: Kubeconfig) -> K8sConfig {
    let mut c = K8sConfig::new(cfg, "ns", fake_scratch_root());
    c.accept_startup_network_window = true;
    c
}

#[test]
fn against_a_fake_cluster_no_gate_and_no_opt_in_is_refused_and_nothing_is_asked_of_the_api() {
    let (fake, cfg) = fake_cluster(vec![node("colima", true, "docker://29")], true);
    match K8sJobExecutor::new(K8sConfig::new(cfg.clone(), "ns", fake_scratch_root())) {
        Err(K8sError::Preflight(m)) => assert!(m.contains("accept_startup_network_window"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(fake.requests().is_empty(), "the refusal happens before any connection: {:?}", fake.requests());
    // The explicit opt-in is accepted by the same cluster.
    assert!(K8sJobExecutor::new(accepting_window(cfg)).is_ok());
}

#[test]
fn a_gate_image_under_the_wrong_digest_is_refused_before_any_connection() {
    let (fake, cfg) = fake_cluster(vec![node("colima", true, "docker://29")], true);
    let mut c = K8sConfig::new(cfg, "ns", fake_scratch_root());
    c.network_gate = Some(av_jobs::k8s::NetworkGate { image: CONTAINER_IMAGE.to_string(), image_digest: format!("sha256:{}", "0".repeat(64)), within: Duration::from_secs(5) });
    match K8sJobExecutor::new(c) {
        Err(K8sError::ImageGate(f)) => assert_eq!(f.kind, pb::JobFailureKind::ExecutorUnavailable as i32, "{f:?}"),
        other => panic!("{other:?}"),
    }
    assert!(fake.requests().is_empty());
}
