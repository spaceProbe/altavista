//! The kubeconfig reader: one context, one cluster, one **client-certificate** user, decoded into
//! memory for [`av_http1::TlsConfig`].
//!
//! # What is accepted, and what is refused (typed)
//!
//! The Jobs executor authenticates with a client certificate and nothing else, and nothing here
//! ever writes a token or a key to a file. So a `users[].user` entry that authenticates any other
//! way is refused with [`KubeconfigError::UserAuthRefused`], naming the method: a bearer `token`,
//! a `tokenFile`, an `exec` credential plugin, an `auth-provider`, `username`/`password`, or
//! impersonation (`as`, `as-groups`, ...; silently ignoring those would run the job as a different
//! identity than the file says). A cluster that disables certificate verification
//! (`insecure-skip-tls-verify: true`), overrides the verified name (`tls-server-name`) or names a
//! `proxy-url` is refused too (`av-http1` has no switch for the first and no override for the
//! other two, deliberately), and so is a `server:` that is not `https://`.
//!
//! The CA and the client certificate and key are read from the `*-data` fields (base64 PEM) or,
//! failing that, from the `certificate-authority` / `client-certificate` / `client-key` file paths
//! (resolved against the kubeconfig's own directory); both forms at once is refused as ambiguous.
//! The server is read from the file every time: the Colima-forwarded port changes between
//! restarts, so nothing here (or in a caller) hard-codes it.
//!
//! # YAML: one function, because the YAML crate is going away (question 246)
//!
//! Question 246 records that the workspace's YAML crate is to be replaced, with this crate as a
//! fifth in the migration's scope; it is used here this round only because it is already locked.
//! So all YAML parsing is the single private function `parse_yaml`, which returns a plain
//! `serde_json::Value`; nothing else in this crate names a type from that crate.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// **The one place this crate touches the YAML crate** (see the module doc, question 246): parses
/// `text` straight into a `serde_json::Value`. Replacing the YAML crate means rewriting this body
/// and the `[dependencies]` line in `Cargo.toml`, nothing else.
fn parse_yaml(text: &str) -> Result<Value, KubeconfigError> {
    serde_yaml::from_str::<Value>(text).map_err(|e| KubeconfigError::Yaml(e.to_string()))
}

/// Why a kubeconfig was not usable. Every variant is a refusal a caller can show a human.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KubeconfigError {
    #[error("cannot read kubeconfig {path:?}: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("neither $KUBECONFIG nor $HOME is set, so there is no default kubeconfig path")]
    NoDefaultPath,
    #[error("$KUBECONFIG names several files ({value:?}); this client reads exactly one kubeconfig file")]
    MultipleFiles { value: String },
    #[error("kubeconfig is not valid YAML: {0}")]
    Yaml(String),
    #[error("kubeconfig has no `current-context` and none was requested")]
    NoCurrentContext,
    #[error("kubeconfig has no context named {0:?}")]
    ContextNotFound(String),
    #[error("context {context:?} names cluster {cluster:?}, which is not in `clusters`")]
    ClusterNotFound { context: String, cluster: String },
    #[error("context {context:?} names user {user:?}, which is not in `users`")]
    UserNotFound { context: String, user: String },
    #[error("cluster {cluster:?} has no `server`")]
    ServerMissing { cluster: String },
    #[error("cluster {cluster:?} server {server:?} is not an https:// URL; this client speaks TLS to the API server only")]
    ServerNotHttps { cluster: String, server: String },
    #[error("cluster {cluster:?} sets insecure-skip-tls-verify; certificate verification is never switched off")]
    InsecureSkipTlsVerify { cluster: String },
    #[error("cluster {cluster:?} sets {field}; this client verifies the server against the host in `server` and has no override or proxy")]
    UnsupportedClusterField { cluster: String, field: &'static str },
    #[error("cluster {cluster:?} has no certificate authority (`certificate-authority-data` or `certificate-authority`); the system store is not trusted for the API server")]
    CaMissing { cluster: String },
    #[error("user {user:?} authenticates by {method}; this client uses a client certificate only")]
    UserAuthRefused { user: String, method: &'static str },
    #[error("user {user:?} has no {field}; a client certificate and key are required")]
    ClientIdentityMissing { user: String, field: &'static str },
    #[error("{field} is given both inline and as a file; give exactly one")]
    AmbiguousSource { field: &'static str },
    #[error("{field} is not valid base64")]
    BadBase64 { field: &'static str },
    #[error("{field} does not hold PEM data of the expected kind ({expected})")]
    NotPem { field: &'static str, expected: &'static str },
}

/// A decoded kubeconfig context: where the API server is and the PEM material to talk to it.
/// `Debug` never prints the private key.
#[derive(Clone, PartialEq, Eq)]
pub struct Kubeconfig {
    /// The context name that was selected.
    pub context: String,
    /// The `https://host:port` of the API server, as the file says.
    pub server: String,
    /// The context's default namespace, if it names one.
    pub namespace: Option<String>,
    pub ca_pem: Vec<u8>,
    pub client_cert_pem: Vec<u8>,
    client_key_pem: Vec<u8>,
}

impl fmt::Debug for Kubeconfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kubeconfig")
            .field("context", &self.context)
            .field("server", &self.server)
            .field("namespace", &self.namespace)
            .field("ca_pem", &format_args!("<{} bytes>", self.ca_pem.len()))
            .field("client_cert_pem", &format_args!("<{} bytes>", self.client_cert_pem.len()))
            .field("client_key_pem", &"<redacted>")
            .finish()
    }
}

impl Kubeconfig {
    /// Reads `$KUBECONFIG` (one file) or `$HOME/.kube/config`, and selects `context` (or the
    /// file's `current-context`).
    pub fn load_default(context: Option<&str>) -> Result<Kubeconfig, KubeconfigError> {
        let path = default_path(std::env::var("KUBECONFIG").ok().as_deref(), std::env::var("HOME").ok().as_deref())?;
        Kubeconfig::load(&path, context)
    }

    /// Reads the file at `path` and selects `context` (or `current-context`).
    pub fn load(path: &Path, context: Option<&str>) -> Result<Kubeconfig, KubeconfigError> {
        let text = fs::read_to_string(path).map_err(|e| KubeconfigError::Unreadable { path: path.to_path_buf(), reason: e.to_string() })?;
        Kubeconfig::parse(&text, path.parent(), context)
    }

    /// Parses kubeconfig YAML. `base_dir` resolves relative `certificate-authority` /
    /// `client-certificate` / `client-key` file paths (None: they must be absolute).
    pub fn parse(yaml: &str, base_dir: Option<&Path>, context: Option<&str>) -> Result<Kubeconfig, KubeconfigError> {
        let doc = parse_yaml(yaml)?;
        let context_name = match context {
            Some(c) => c.to_string(),
            None => str_field(&doc, "current-context").ok_or(KubeconfigError::NoCurrentContext)?.to_string(),
        };
        let ctx = named(&doc, "contexts", &context_name).ok_or_else(|| KubeconfigError::ContextNotFound(context_name.clone()))?;
        let ctx = ctx.get("context").unwrap_or(&Value::Null);
        let cluster_name = str_field(ctx, "cluster").unwrap_or_default().to_string();
        let user_name = str_field(ctx, "user").unwrap_or_default().to_string();
        let namespace = str_field(ctx, "namespace").map(str::to_string);

        let cluster = named(&doc, "clusters", &cluster_name)
            .ok_or_else(|| KubeconfigError::ClusterNotFound { context: context_name.clone(), cluster: cluster_name.clone() })?;
        let cluster = cluster.get("cluster").unwrap_or(&Value::Null);
        let user = named(&doc, "users", &user_name).ok_or_else(|| KubeconfigError::UserNotFound { context: context_name.clone(), user: user_name.clone() })?;
        let user = user.get("user").unwrap_or(&Value::Null);

        // ---- the cluster: server, verification switches, CA ----
        let server = str_field(cluster, "server").ok_or_else(|| KubeconfigError::ServerMissing { cluster: cluster_name.clone() })?.to_string();
        if !server.to_ascii_lowercase().starts_with("https://") {
            return Err(KubeconfigError::ServerNotHttps { cluster: cluster_name, server });
        }
        if cluster.get("insecure-skip-tls-verify").and_then(Value::as_bool) == Some(true) {
            return Err(KubeconfigError::InsecureSkipTlsVerify { cluster: cluster_name });
        }
        for field in ["tls-server-name", "proxy-url"] {
            if cluster.get(field).is_some_and(|v| !v.is_null()) {
                return Err(KubeconfigError::UnsupportedClusterField { cluster: cluster_name, field });
            }
        }

        // ---- the user: refuse every authentication method but a client certificate ----
        for (key, method) in [
            ("token", "a bearer token"),
            ("tokenFile", "a token file"),
            ("exec", "an exec credential plugin"),
            ("auth-provider", "an auth provider"),
            ("username", "a username and password"),
            ("password", "a username and password"),
            ("as", "impersonation (`as`)"),
            ("as-uid", "impersonation (`as-uid`)"),
            ("as-groups", "impersonation (`as-groups`)"),
            ("as-user-extra", "impersonation (`as-user-extra`)"),
        ] {
            if user.get(key).is_some_and(|v| !v.is_null()) {
                return Err(KubeconfigError::UserAuthRefused { user: user_name, method });
            }
        }

        let base = base_dir;
        let ca_pem = pem_source(cluster, "certificate-authority-data", "certificate-authority", base, "CERTIFICATE")?
            .ok_or(KubeconfigError::CaMissing { cluster: cluster_name })?;
        let client_cert_pem = pem_source(user, "client-certificate-data", "client-certificate", base, "CERTIFICATE")?
            .ok_or_else(|| KubeconfigError::ClientIdentityMissing { user: user_name.clone(), field: "client-certificate-data" })?;
        let client_key_pem = pem_source(user, "client-key-data", "client-key", base, "PRIVATE KEY")?
            .ok_or(KubeconfigError::ClientIdentityMissing { user: user_name, field: "client-key-data" })?;

        Ok(Kubeconfig { context: context_name, server, namespace, ca_pem, client_cert_pem, client_key_pem })
    }

    /// The TLS configuration for `av-http1`: this cluster's CA (replacing the system store) and
    /// the client identity. Hostname / IP-SAN verification is always on in `av-http1`.
    pub fn tls_config(&self) -> av_http1::TlsConfig {
        av_http1::TlsConfig::new().with_ca_pem(self.ca_pem.clone()).with_client_identity(self.client_cert_pem.clone(), self.client_key_pem.clone())
    }
}

/// `$KUBECONFIG` (exactly one path) or `$HOME/.kube/config`.
fn default_path(kubeconfig_env: Option<&str>, home: Option<&str>) -> Result<PathBuf, KubeconfigError> {
    match kubeconfig_env.filter(|v| !v.is_empty()) {
        Some(v) if v.contains(':') => Err(KubeconfigError::MultipleFiles { value: v.to_string() }),
        Some(v) => Ok(PathBuf::from(v)),
        None => home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".kube").join("config")).ok_or(KubeconfigError::NoDefaultPath),
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The entry of the list `list` whose `name` is `name`.
fn named<'a>(doc: &'a Value, list: &str, name: &str) -> Option<&'a Value> {
    doc.get(list)?.as_array()?.iter().find(|e| e.get("name").and_then(Value::as_str) == Some(name))
}

/// PEM bytes from `inline_key` (base64) or `file_key` (a path); `Ok(None)` when neither is set.
fn pem_source(section: &Value, inline_key: &'static str, file_key: &'static str, base: Option<&Path>, kind: &'static str) -> Result<Option<Vec<u8>>, KubeconfigError> {
    let inline = str_field(section, inline_key);
    let file = str_field(section, file_key);
    let bytes = match (inline, file) {
        (Some(_), Some(_)) => return Err(KubeconfigError::AmbiguousSource { field: inline_key }),
        (None, None) => return Ok(None),
        (Some(b64), None) => {
            let compact: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            openssl::base64::decode_block(&compact).map_err(|_| KubeconfigError::BadBase64 { field: inline_key })?
        }
        (None, Some(path)) => {
            let p = Path::new(path);
            let full = if p.is_absolute() { p.to_path_buf() } else { base.map(|b| b.join(p)).unwrap_or_else(|| p.to_path_buf()) };
            fs::read(&full).map_err(|e| KubeconfigError::Unreadable { path: full, reason: e.to_string() })?
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let ok = text.contains("-----BEGIN ") && text.contains(kind) && text.contains("-----END ");
    if !ok {
        return Err(KubeconfigError::NotPem { field: inline_key, expected: kind });
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
    const KEY: &str = "-----BEGIN EC PRIVATE KEY-----\nMHcC\n-----END EC PRIVATE KEY-----\n";

    fn b64(s: &str) -> String {
        openssl::base64::encode_block(s.as_bytes())
    }

    /// A well-formed kubeconfig with a user/cluster body spliced in.
    fn config(cluster_extra: &str, user_body: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: colima\n\
             clusters:\n- name: colima-cluster\n  cluster:\n    server: https://127.0.0.1:50839\n    certificate-authority-data: {ca}\n{cluster_extra}\
             contexts:\n- name: colima\n  context: {{cluster: colima-cluster, user: colima-user, namespace: av}}\n\
             - name: other\n  context: {{cluster: colima-cluster, user: missing-user}}\n\
             users:\n- name: colima-user\n  user:\n{user_body}",
            ca = b64(CERT)
        )
    }

    fn good_user() -> String {
        format!("    client-certificate-data: {}\n    client-key-data: {}\n", b64(CERT), b64(KEY))
    }

    #[test]
    fn a_client_certificate_kubeconfig_is_decoded_into_memory() {
        let cfg = Kubeconfig::parse(&config("", &good_user()), None, None).unwrap();
        assert_eq!(cfg.context, "colima");
        assert_eq!(cfg.server, "https://127.0.0.1:50839");
        assert_eq!(cfg.namespace.as_deref(), Some("av"));
        assert_eq!(cfg.ca_pem, CERT.as_bytes());
        assert_eq!(cfg.client_cert_pem, CERT.as_bytes());
        assert_eq!(cfg.client_key_pem, KEY.as_bytes());
        // The key never reaches Debug output.
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("MHcC") && dbg.contains("redacted"), "{dbg}");
    }

    #[test]
    fn base64_with_line_wrapping_decodes() {
        let wrapped = format!("|\n      {}\n      {}", &b64(CERT)[..8], &b64(CERT)[8..]);
        let yaml = config("", &good_user()).replace(&b64(CERT), &wrapped);
        // The first occurrence is the CA (inside `clusters`), the others are the user's.
        let cfg = Kubeconfig::parse(&yaml, None, None);
        assert!(cfg.is_ok(), "{cfg:?}");
    }

    #[test]
    fn an_explicit_context_wins_and_unknown_names_are_typed() {
        assert_eq!(Kubeconfig::parse(&config("", &good_user()), None, Some("nope")), Err(KubeconfigError::ContextNotFound("nope".into())));
        assert_eq!(
            Kubeconfig::parse(&config("", &good_user()), None, Some("other")),
            Err(KubeconfigError::UserNotFound { context: "other".into(), user: "missing-user".into() })
        );
        let no_current = config("", &good_user()).replace("current-context: colima\n", "");
        assert_eq!(Kubeconfig::parse(&no_current, None, None), Err(KubeconfigError::NoCurrentContext));
        let no_cluster = config("", &good_user()).replace("- name: colima-cluster", "- name: renamed");
        assert!(matches!(Kubeconfig::parse(&no_cluster, None, None), Err(KubeconfigError::ClusterNotFound { .. })));
    }

    #[test]
    fn every_non_certificate_authentication_method_is_refused_by_name() {
        for (body, method) in [
            ("    token: abc\n", "a bearer token"),
            ("    tokenFile: /var/run/token\n", "a token file"),
            ("    exec:\n      command: aws\n", "an exec credential plugin"),
            ("    auth-provider:\n      name: gcp\n", "an auth provider"),
            ("    username: admin\n", "a username and password"),
            ("    password: hunter2\n", "a username and password"),
            ("    as: someone\n", "impersonation (`as`)"),
        ] {
            // Refused even when a client certificate is also present.
            let yaml = config("", &format!("{body}{}", good_user()));
            match Kubeconfig::parse(&yaml, None, None) {
                Err(KubeconfigError::UserAuthRefused { user, method: m }) => assert_eq!((user.as_str(), m), ("colima-user", method)),
                other => panic!("{body:?}: expected UserAuthRefused, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_non_https_server_and_every_verification_override_are_refused() {
        let http = config("", &good_user()).replace("https://127.0.0.1:50839", "http://127.0.0.1:8080");
        assert!(matches!(Kubeconfig::parse(&http, None, None), Err(KubeconfigError::ServerNotHttps { .. })));
        let bare = config("", &good_user()).replace("https://127.0.0.1:50839", "127.0.0.1:6443");
        assert!(matches!(Kubeconfig::parse(&bare, None, None), Err(KubeconfigError::ServerNotHttps { .. })));
        assert!(matches!(
            Kubeconfig::parse(&config("    insecure-skip-tls-verify: true\n", &good_user()), None, None),
            Err(KubeconfigError::InsecureSkipTlsVerify { .. })
        ));
        // An explicit `false` is just the default and is accepted.
        assert!(Kubeconfig::parse(&config("    insecure-skip-tls-verify: false\n", &good_user()), None, None).is_ok());
        assert!(matches!(
            Kubeconfig::parse(&config("    tls-server-name: other.example\n", &good_user()), None, None),
            Err(KubeconfigError::UnsupportedClusterField { field: "tls-server-name", .. })
        ));
        assert!(matches!(
            Kubeconfig::parse(&config("    proxy-url: http://proxy:3128\n", &good_user()), None, None),
            Err(KubeconfigError::UnsupportedClusterField { field: "proxy-url", .. })
        ));
        let no_server = config("", &good_user()).replace("    server: https://127.0.0.1:50839\n", "");
        assert!(matches!(Kubeconfig::parse(&no_server, None, None), Err(KubeconfigError::ServerMissing { .. })));
    }

    #[test]
    fn a_missing_ca_or_client_identity_is_typed() {
        let no_ca = config("", &good_user()).replacen(&format!("    certificate-authority-data: {}\n", b64(CERT)), "", 1);
        assert!(matches!(Kubeconfig::parse(&no_ca, None, None), Err(KubeconfigError::CaMissing { .. })));
        let cert_only = format!("    client-certificate-data: {}\n", b64(CERT));
        assert_eq!(
            Kubeconfig::parse(&config("", &cert_only), None, None),
            Err(KubeconfigError::ClientIdentityMissing { user: "colima-user".into(), field: "client-key-data" })
        );
        assert_eq!(
            Kubeconfig::parse(&config("", ""), None, None),
            Err(KubeconfigError::ClientIdentityMissing { user: "colima-user".into(), field: "client-certificate-data" })
        );
    }

    #[test]
    fn bad_base64_and_non_pem_data_are_typed() {
        let bad = format!("    client-certificate-data: '!!!notbase64'\n    client-key-data: {}\n", b64(KEY));
        assert_eq!(Kubeconfig::parse(&config("", &bad), None, None), Err(KubeconfigError::BadBase64 { field: "client-certificate-data" }));
        let not_pem = format!("    client-certificate-data: {}\n    client-key-data: {}\n", b64("hello"), b64(KEY));
        assert_eq!(
            Kubeconfig::parse(&config("", &not_pem), None, None),
            Err(KubeconfigError::NotPem { field: "client-certificate-data", expected: "CERTIFICATE" })
        );
        // A certificate where the key belongs is not a private key.
        let swapped = format!("    client-certificate-data: {}\n    client-key-data: {}\n", b64(CERT), b64(CERT));
        assert_eq!(Kubeconfig::parse(&config("", &swapped), None, None), Err(KubeconfigError::NotPem { field: "client-key-data", expected: "PRIVATE KEY" }));
    }

    #[test]
    fn inline_and_file_forms_are_exclusive_and_files_resolve_against_the_config_dir() {
        let both = format!("    client-certificate-data: {}\n    client-certificate: c.pem\n    client-key-data: {}\n", b64(CERT), b64(KEY));
        assert_eq!(Kubeconfig::parse(&config("", &both), None, None), Err(KubeconfigError::AmbiguousSource { field: "client-certificate-data" }));

        let dir = std::env::temp_dir().join(format!("av-jobs-kubeconfig-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("c.pem"), CERT).unwrap();
        fs::write(dir.join("k.pem"), KEY).unwrap();
        let files = "    client-certificate: c.pem\n    client-key: k.pem\n";
        let cfg = Kubeconfig::parse(&config("", files), Some(&dir), None);
        let missing = Kubeconfig::parse(&config("", "    client-certificate: nope.pem\n    client-key: k.pem\n"), Some(&dir), None);
        let _ = fs::remove_dir_all(&dir);
        let cfg = cfg.unwrap();
        assert_eq!((cfg.client_cert_pem.as_slice(), cfg.client_key_pem.as_slice()), (CERT.as_bytes(), KEY.as_bytes()));
        assert!(matches!(missing, Err(KubeconfigError::Unreadable { .. })), "{missing:?}");
    }

    #[test]
    fn the_default_path_is_kubeconfig_env_one_file_or_home_dot_kube_config() {
        assert_eq!(default_path(Some("/a/b"), Some("/h")).unwrap(), PathBuf::from("/a/b"));
        assert_eq!(default_path(None, Some("/h")).unwrap(), PathBuf::from("/h/.kube/config"));
        assert_eq!(default_path(Some(""), Some("/h")).unwrap(), PathBuf::from("/h/.kube/config"));
        assert_eq!(default_path(Some("/a:/b"), Some("/h")), Err(KubeconfigError::MultipleFiles { value: "/a:/b".into() }));
        assert_eq!(default_path(None, None), Err(KubeconfigError::NoDefaultPath));
    }

    #[test]
    fn invalid_yaml_and_a_missing_file_are_typed() {
        assert!(matches!(Kubeconfig::parse("a: [unclosed", None, None), Err(KubeconfigError::Yaml(_))));
        assert!(matches!(Kubeconfig::load(Path::new("/nonexistent/av-kubeconfig"), None), Err(KubeconfigError::Unreadable { .. })));
    }
}
