//! The Kubernetes API client the Jobs executor uses: a thin, typed layer over [`av_http1`].
//!
//! Only what the executor and its test harness need: create / get / delete a `batch/v1` Job, list
//! and delete pods, read a pod's container log, list nodes, and create / delete a namespace.
//! Bodies are `serde_json::Value`s (the executor reads the handful of fields it needs; it does not
//! model the whole API). One connection per request (an `av-http1` property), so a [`K8sApi`] is
//! `Send + Sync` and any number of threads can share it.
//!
//! # Errors
//!
//! A transport failure is [`ApiError::Unreachable`] (connect, DNS, timeouts and I/O: the API
//! server could not be talked to) or [`ApiError::Tls`] (the handshake or certificate verification
//! failed: never retried, never relaxed). A non-2xx reply is [`ApiError::Refused`] carrying the
//! Kubernetes `Status` object's `code`, `reason` and `message` (a reply that is not a `Status`,
//! e.g. a proxy's HTML page, is typed the same way from the HTTP status line and a bounded
//! excerpt of the body). A 2xx reply whose body is not the JSON the call expects is
//! [`ApiError::BadBody`].

use std::collections::BTreeMap;

use av_http1::{Client, Endpoint, HttpError, Request, Response, Timeouts};
use serde_json::{json, Value};

use super::kubeconfig::Kubeconfig;

/// The Kubernetes `Status` object an error reply carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiStatus {
    /// The HTTP status code (the `Status`' `code` when present, else the response's).
    pub code: u16,
    /// The machine-readable `reason` (`NotFound`, `AlreadyExists`, `Forbidden`, `Invalid`, ...);
    /// empty if the reply was not a `Status`.
    pub reason: String,
    /// The human-readable `message`.
    pub message: String,
}

impl std::fmt::Display for ApiStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.code, if self.reason.is_empty() { "(no reason)" } else { &self.reason }, self.message)
    }
}

impl ApiStatus {
    /// Types a non-2xx response: a `Status` JSON body if there is one, else the HTTP line plus an
    /// excerpt of whatever the body was.
    pub fn from_response(response: &Response) -> ApiStatus {
        if let Ok(v) = serde_json::from_slice::<Value>(&response.body) {
            if v.get("kind").and_then(Value::as_str) == Some("Status") {
                return ApiStatus {
                    code: v.get("code").and_then(Value::as_u64).and_then(|c| u16::try_from(c).ok()).unwrap_or(response.status),
                    reason: v.get("reason").and_then(Value::as_str).unwrap_or_default().to_string(),
                    message: v.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
                };
            }
        }
        let excerpt: String = String::from_utf8_lossy(&response.body).chars().take(300).collect();
        ApiStatus { code: response.status, reason: String::new(), message: format!("{} (not a Kubernetes Status): {}", response.reason, excerpt.trim()) }
    }
}

/// Why an API call did not return what was asked for.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The API server could not be reached or stopped answering (connect, DNS, timeout, I/O).
    #[error("Kubernetes API unreachable: {0}")]
    Unreachable(String),
    /// TLS failed (handshake, certificate or hostname verification). Never retried or relaxed.
    #[error("Kubernetes API TLS failure: {0}")]
    Tls(String),
    /// The request or the client's configuration was refused before or by the transport layer
    /// (invalid endpoint, invalid TLS configuration, a malformed or oversized response).
    #[error("Kubernetes API transport error: {0}")]
    Transport(String),
    /// The API server answered with a non-2xx status.
    #[error("Kubernetes API refused the request: {0}")]
    Refused(ApiStatus),
    /// A 2xx answer whose body was not the expected JSON.
    #[error("Kubernetes API answered 2xx with an unexpected body ({what}): {detail}")]
    BadBody { what: &'static str, detail: String },
}

impl ApiError {
    fn from_http(e: HttpError) -> ApiError {
        match e {
            HttpError::Unreachable { .. } | HttpError::Timeout { .. } | HttpError::Io { .. } => ApiError::Unreachable(e.to_string()),
            HttpError::Tls { .. } => ApiError::Tls(e.to_string()),
            other => ApiError::Transport(other.to_string()),
        }
    }

    /// Whether the server said 404.
    pub fn is_not_found(&self) -> bool {
        matches!(self, ApiError::Refused(s) if s.code == 404)
    }

    /// Whether the server said 409.
    pub fn is_conflict(&self) -> bool {
        matches!(self, ApiError::Refused(s) if s.code == 409)
    }
}

/// A client for one API server.
#[derive(Debug, Clone)]
pub struct K8sApi {
    client: Client,
}

/// Percent-encodes everything but RFC 3986 unreserved characters (for query values).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl K8sApi {
    /// Builds a client for `config.server` with the kubeconfig's CA and client identity.
    pub fn new(config: &Kubeconfig, timeouts: Timeouts) -> Result<K8sApi, ApiError> {
        let endpoint = Endpoint::parse(&config.server).map_err(ApiError::from_http)?;
        let client = Client::new(endpoint, Some(config.tls_config()), timeouts).map_err(ApiError::from_http)?;
        Ok(K8sApi { client })
    }

    /// Wraps an existing `av-http1` client (tests that point at a fake server).
    pub fn from_client(client: Client) -> K8sApi {
        K8sApi { client }
    }

    fn send(&self, request: Request) -> Result<Response, ApiError> {
        let response = self.client.request(request).map_err(ApiError::from_http)?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(ApiError::Refused(ApiStatus::from_response(&response)))
        }
    }

    fn json_call(&self, request: Request, what: &'static str) -> Result<Value, ApiError> {
        let response = self.send(request.header("Accept", "application/json"))?;
        serde_json::from_slice(&response.body).map_err(|e| ApiError::BadBody { what, detail: e.to_string() })
    }

    fn json_post(path: String, body: &Value) -> Request {
        Request::post(path).header("Content-Type", "application/json").body(body.to_string().into_bytes())
    }

    /// `POST /api/v1/namespaces` with the given labels.
    pub fn create_namespace(&self, name: &str, labels: &BTreeMap<String, String>) -> Result<Value, ApiError> {
        let body = json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name, "labels": labels}});
        self.json_call(Self::json_post("/api/v1/namespaces".to_string(), &body), "create namespace")
    }

    /// `DELETE /api/v1/namespaces/{name}` (asynchronous: the namespace goes `Terminating`).
    pub fn delete_namespace(&self, name: &str) -> Result<(), ApiError> {
        self.send(Request::delete(format!("/api/v1/namespaces/{}", pct(name))).header("Accept", "application/json")).map(|_| ())
    }

    /// `GET /api/v1/namespaces/{name}`.
    pub fn get_namespace(&self, name: &str) -> Result<Value, ApiError> {
        self.json_call(Request::get(format!("/api/v1/namespaces/{}", pct(name))), "get namespace")
    }

    /// `GET /api/v1/nodes`: the `items`.
    pub fn list_nodes(&self) -> Result<Vec<Value>, ApiError> {
        items(self.json_call(Request::get("/api/v1/nodes"), "list nodes")?, "list nodes")
    }

    /// `POST /apis/batch/v1/namespaces/{ns}/jobs`.
    pub fn create_job(&self, namespace: &str, job: &Value) -> Result<Value, ApiError> {
        self.json_call(Self::json_post(format!("/apis/batch/v1/namespaces/{}/jobs", pct(namespace)), job), "create job")
    }

    /// `GET /apis/batch/v1/namespaces/{ns}/jobs/{name}`.
    pub fn get_job(&self, namespace: &str, name: &str) -> Result<Value, ApiError> {
        self.json_call(Request::get(format!("/apis/batch/v1/namespaces/{}/jobs/{}", pct(namespace), pct(name))), "get job")
    }

    /// `DELETE` the Job with `propagationPolicy: Foreground` (its pods are deleted first and the
    /// Job object disappears only when they are gone) and `gracePeriodSeconds: 0` on the Job
    /// itself. A 404 is `Err` ([`ApiError::is_not_found`]) so the caller decides whether that
    /// matters.
    pub fn delete_job(&self, namespace: &str, name: &str) -> Result<(), ApiError> {
        let body = json!({"apiVersion": "v1", "kind": "DeleteOptions", "propagationPolicy": "Foreground", "gracePeriodSeconds": 0});
        let request = Request::delete(format!("/apis/batch/v1/namespaces/{}/jobs/{}", pct(namespace), pct(name)))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string().into_bytes());
        self.send(request).map(|_| ())
    }

    /// `GET /api/v1/namespaces/{ns}/services/{name}`.
    pub fn get_service(&self, namespace: &str, name: &str) -> Result<Value, ApiError> {
        self.json_call(Request::get(format!("/api/v1/namespaces/{}/services/{}", pct(namespace), pct(name))), "get service")
    }

    /// `GET /apis/networking.k8s.io/v1/namespaces/{ns}/networkpolicies/{name}`.
    pub fn get_network_policy(&self, namespace: &str, name: &str) -> Result<Value, ApiError> {
        self.json_call(Request::get(format!("/apis/networking.k8s.io/v1/namespaces/{}/networkpolicies/{}", pct(namespace), pct(name))), "get network policy")
    }

    /// `POST /apis/networking.k8s.io/v1/namespaces/{ns}/networkpolicies`.
    pub fn create_network_policy(&self, namespace: &str, policy: &Value) -> Result<Value, ApiError> {
        self.json_call(Self::json_post(format!("/apis/networking.k8s.io/v1/namespaces/{}/networkpolicies", pct(namespace)), policy), "create network policy")
    }

    /// `DELETE` a network policy (the fail-closed proof removes the executor's own, in a test namespace).
    pub fn delete_network_policy(&self, namespace: &str, name: &str) -> Result<(), ApiError> {
        let request = Request::delete(format!("/apis/networking.k8s.io/v1/namespaces/{}/networkpolicies/{}", pct(namespace), pct(name))).header("Accept", "application/json");
        self.send(request).map(|_| ())
    }

    /// `POST /api/v1/namespaces/{ns}/pods` (the network-enforcement probes create bare pods).
    pub fn create_pod(&self, namespace: &str, pod: &Value) -> Result<Value, ApiError> {
        self.json_call(Self::json_post(format!("/api/v1/namespaces/{}/pods", pct(namespace)), pod), "create pod")
    }

    /// `GET /api/v1/namespaces/{ns}/pods/{name}`.
    pub fn get_pod(&self, namespace: &str, name: &str) -> Result<Value, ApiError> {
        self.json_call(Request::get(format!("/api/v1/namespaces/{}/pods/{}", pct(namespace), pct(name))), "get pod")
    }

    /// `GET .../pods?labelSelector=...`: the `items`.
    pub fn list_pods(&self, namespace: &str, label_selector: &str) -> Result<Vec<Value>, ApiError> {
        let path = format!("/api/v1/namespaces/{}/pods?labelSelector={}", pct(namespace), pct(label_selector));
        items(self.json_call(Request::get(path), "list pods")?, "list pods")
    }

    /// `DELETE` one pod (`grace_seconds` 0 = immediately).
    pub fn delete_pod(&self, namespace: &str, name: &str, grace_seconds: u32) -> Result<(), ApiError> {
        let body = json!({"apiVersion": "v1", "kind": "DeleteOptions", "gracePeriodSeconds": grace_seconds});
        let request = Request::delete(format!("/api/v1/namespaces/{}/pods/{}", pct(namespace), pct(name)))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string().into_bytes());
        self.send(request).map(|_| ())
    }

    /// `GET .../pods/{pod}/log`: at most the last `tail_lines` lines, and at most `limit_bytes`
    /// bytes of those (the API's `limitBytes` keeps the *start* of the tail window).
    pub fn pod_log(&self, namespace: &str, pod: &str, container: &str, tail_lines: u64, limit_bytes: u64) -> Result<Vec<u8>, ApiError> {
        let path = format!(
            "/api/v1/namespaces/{}/pods/{}/log?container={}&tailLines={tail_lines}&limitBytes={limit_bytes}",
            pct(namespace),
            pct(pod),
            pct(container)
        );
        Ok(self.send(Request::get(path))?.body)
    }
}

fn items(list: Value, what: &'static str) -> Result<Vec<Value>, ApiError> {
    match list.get("items") {
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(Value::Null) | None => Err(ApiError::BadBody { what, detail: "no `items` array".to_string() }),
        Some(other) => Err(ApiError::BadBody { what, detail: format!("`items` is not an array: {other}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, reason: &str, body: &str) -> Response {
        Response { status, reason: reason.to_string(), headers: vec![], body: body.as_bytes().to_vec() }
    }

    #[test]
    fn a_status_body_is_typed_with_code_reason_and_message() {
        let r = response(
            409,
            "Conflict",
            r#"{"kind":"Status","apiVersion":"v1","metadata":{},"status":"Failure","message":"jobs.batch \"x\" already exists","reason":"AlreadyExists","details":{"name":"x"},"code":409}"#,
        );
        let s = ApiStatus::from_response(&r);
        assert_eq!((s.code, s.reason.as_str()), (409, "AlreadyExists"));
        assert_eq!(s.message, "jobs.batch \"x\" already exists");
        assert!(ApiError::Refused(s).is_conflict());
    }

    #[test]
    fn a_non_status_body_is_typed_from_the_http_line() {
        let s = ApiStatus::from_response(&response(502, "Bad Gateway", "<html>proxy said no</html>"));
        assert_eq!((s.code, s.reason.as_str()), (502, ""));
        assert!(s.message.contains("Bad Gateway") && s.message.contains("proxy said no"), "{s}");
        // A JSON body of another kind is not trusted as a Status either.
        let s = ApiStatus::from_response(&response(500, "Internal Server Error", r#"{"kind":"Pod"}"#));
        assert_eq!(s.reason, "");
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(pct("batch.kubernetes.io/job-name=av-job-1"), "batch.kubernetes.io%2Fjob-name%3Dav-job-1");
        assert_eq!(pct("a b#c"), "a%20b%23c");
    }

    #[test]
    fn items_requires_an_array() {
        assert_eq!(items(json!({"items": [1, 2]}), "t").unwrap().len(), 2);
        assert!(matches!(items(json!({"items": null}), "t"), Err(ApiError::BadBody { .. })));
        assert!(matches!(items(json!({}), "t"), Err(ApiError::BadBody { .. })));
        assert!(matches!(items(json!({"items": {}}), "t"), Err(ApiError::BadBody { .. })));
    }
}
