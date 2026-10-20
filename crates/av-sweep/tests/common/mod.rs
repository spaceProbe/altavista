//! The real-container fixture shared by `tests/clickhouse_store.rs` and
//! `tests/clickhouse_real_sweep.rs`: one labelled, loopback-only ClickHouse container from the
//! image `services/clickhouse/IMAGE_DIGEST.md` records, with the same gate / lock / label / prune
//! discipline as `crates/av-store/tests/s3_store.rs` (the template this follows):
//!
//! 1. [`gate`] compares the image already present locally with the recorded digest
//!    (`av_lockstep::docker::recorded_digest_gate`, which only ever runs `docker image inspect`);
//!    a failed gate prints a visible `SKIPPED ...` line (`announce_gate_skip`) and the test
//!    returns. **No test here ever pulls** (questions 154, 212(a)); no image is referenced by a
//!    floating tag.
//! 2. Every test takes `lock_docker_tests()` inside its own body, then
//!    `prune_stale_test_resources` before creating anything.
//! 3. Every container carries `av.test=1` and a run id label (`test_label_args`).
//! 4. [`ChServer`]'s `Drop` removes the container **and its volumes** (`docker rm -fv`), even on a
//!    panic. The image declares `VOLUME /var/lib/clickhouse`, so a plain `docker rm` would leave
//!    an anonymous volume behind (measured, `services/clickhouse/IMAGE_DIGEST.md`); the container
//!    is also started with `--tmpfs /var/lib/clickhouse`, so none is created in the first place.
//!    This is why the fixture runs `docker` itself instead of `ManagedContainer::run_local`, which
//!    can neither mount a tmpfs nor remove volumes.
//! 5. The RUNNING container's image id is compared with the recorded digest before it is trusted.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use av_lockstep::docker::{announce_gate_skip, recorded_digest_gate, test_label_args, test_run_id, DockerGateReason};
use av_sweep::clickhouse::{ChClient, ClickHouseConfig};

pub const HTTP_PORT: u16 = 8123;
pub const HTTPS_PORT: u16 = 8443;
const READY_TIMEOUT: Duration = Duration::from_secs(90);

pub fn image_digest_md_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/clickhouse/IMAGE_DIGEST.md")
}

fn fenced_block_after(text: &str, marker: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let marker_idx = lines.iter().position(|l| l.contains(marker))?;
    let fence_start = marker_idx + lines[marker_idx..].iter().position(|l| l.trim() == "```")?;
    let fence_end = fence_start + 1 + lines[fence_start + 1..].iter().position(|l| l.trim() == "```")?;
    let content = lines[fence_start + 1..fence_end].join("\n").trim().to_string();
    (!content.is_empty()).then_some(content)
}

/// `(image_ref, recorded_local_image_id)` read from `services/clickhouse/IMAGE_DIGEST.md` at test
/// time: the recorded digest has exactly one home.
pub fn parse_image_digest_md() -> (String, String) {
    let path = image_digest_md_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read {path:?}: {e}"));
    let image_ref = fenced_block_after(&text, "Registry reference").unwrap_or_else(|| panic!("no 'Registry reference' fenced block in {path:?}"));
    assert!(image_ref.contains("@sha256:"), "the recorded image reference must be by digest, never a floating tag: {image_ref}");
    let recorded_id = fenced_block_after(&text, "docker image inspect").unwrap_or_else(|| panic!("no 'docker image inspect' fenced block in {path:?}"));
    (image_ref, recorded_id)
}

pub fn gate() -> Result<(), DockerGateReason> {
    let (image_ref, recorded_id) = parse_image_digest_md();
    recorded_digest_gate(&image_ref, &recorded_id)
}

/// First line of every real-container test: a visible skip, never a silent pass.
macro_rules! gate_or_skip {
    ($test_name:expr) => {
        if let Err(reason) = $crate::common::gate() {
            let line = av_lockstep::docker::announce_gate_skip($test_name, &reason);
            assert!(line.starts_with("SKIPPED "), "{line:?}");
            return;
        }
    };
}

fn docker(args: &[&str]) -> Result<String, String> {
    let output = Command::new("docker").args(args).output().map_err(|e| format!("could not launch `docker {args:?}`: {e}"))?;
    if !output.status.success() {
        return Err(format!("`docker {args:?}` failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn docker_ok(args: &[&str]) -> String {
    docker(args).unwrap_or_else(|e| panic!("{e}"))
}

/// Files `docker cp`'d into the container between `create` and `start` (no bind mount: Colima
/// mounts only `$HOME`, and the copy needs no mount at all).
pub struct CopyIn {
    pub host_path: PathBuf,
    pub container_path: String,
}

#[derive(Default)]
pub struct StartOpts {
    /// Publish the HTTPS port too (the container then needs a `config.d` file enabling it).
    pub https: bool,
    pub copy_in: Vec<CopyIn>,
    /// `CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1`, so SQL `CREATE USER` / `GRANT` work for the
    /// image-created user (the read-only-user tests need it).
    pub access_management: bool,
}

/// One running ClickHouse container. Removed (with its volumes) on drop.
pub struct ChServer {
    pub container_id: String,
    pub http_port: u16,
    pub https_port: Option<u16>,
    pub database: String,
    pub user: String,
    pub password: String,
    /// The environment variable (set in this process) holding `password`.
    pub password_var: String,
    pub run_id: String,
    pub image_ref: String,
}

fn sanitize(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect()
}

impl ChServer {
    /// Starts the server. The caller must already hold `lock_docker_tests()` and have called
    /// `prune_stale_test_resources`.
    pub fn start(test_name: &str, opts: StartOpts) -> ChServer {
        let (image_ref, recorded_id) = parse_image_digest_md();
        let run_id = test_run_id();
        let tag = sanitize(&run_id);
        let user = format!("avuser{tag}");
        let password = format!("avpw{tag}x");
        let database = "avstudies".to_string();
        let password_var = format!("AV_CH_TEST_PASSWORD_{tag}");
        std::env::set_var(&password_var, &password);

        let mut args: Vec<String> = vec!["create".into()];
        args.extend(test_label_args(&run_id));
        args.extend(["--tmpfs".into(), "/var/lib/clickhouse".into(), "-p".into(), format!("127.0.0.1::{HTTP_PORT}")]);
        if opts.https {
            args.extend(["-p".into(), format!("127.0.0.1::{HTTPS_PORT}")]);
        }
        args.extend(["-e".into(), format!("CLICKHOUSE_USER={user}"), "-e".into(), format!("CLICKHOUSE_PASSWORD={password}"), "-e".into(), format!("CLICKHOUSE_DB={database}")]);
        if opts.access_management {
            args.extend(["-e".into(), "CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1".into()]);
        }
        args.push(image_ref.clone());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let container_id = docker_ok(&arg_refs);
        let mut server = ChServer { container_id: container_id.clone(), http_port: 0, https_port: None, database, user, password, password_var, run_id, image_ref };

        for copy in &opts.copy_in {
            docker_ok(&["cp", copy.host_path.to_str().expect("utf-8 path"), &format!("{container_id}:{}", copy.container_path)]);
        }
        docker_ok(&["start", &container_id]);

        let port_of = |container_port: u16| -> u16 {
            let raw = docker_ok(&["port", &container_id, &format!("{container_port}/tcp")]);
            raw.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("could not parse `docker port` output {raw:?}"))
        };
        server.http_port = port_of(HTTP_PORT);
        if opts.https {
            server.https_port = Some(port_of(HTTPS_PORT));
        }

        // The running container's image id must equal the recorded digest (question 212(a)).
        let running_image = docker_ok(&["inspect", "--format", "{{.Image}}", &container_id]);
        assert_eq!(running_image, recorded_id, "{test_name}: the RUNNING container's image id must equal services/clickhouse/IMAGE_DIGEST.md's record");

        server.wait_ready(test_name);
        server
    }

    pub fn http_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    pub fn https_url(&self, host: &str) -> String {
        format!("https://{host}:{}", self.https_port.expect("started with https"))
    }

    /// The plaintext config for the image-created user, password by environment variable.
    pub fn config(&self) -> ClickHouseConfig {
        let mut config = ClickHouseConfig::new(self.http_url(), self.database.clone(), self.user.clone()).with_password_env(self.password_var.clone());
        config.timeouts = av_http1::Timeouts::uniform(Duration::from_secs(20));
        config
    }

    /// A raw client for administration (DDL, `CREATE USER`, mutations, `SHOW CREATE TABLE`).
    pub fn admin(&self) -> ChClient {
        ChClient::connect(&self.config()).expect("admin client")
    }

    #[allow(clippy::result_large_err)]
    fn wait_ready(&self, test_name: &str) {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut last;
        loop {
            let attempt = ChClient::connect(&self.config()).and_then(|c| {
                c.ping()?;
                c.query("SELECT 1 FORMAT RowBinary", &[], "readiness probe")
            });
            match attempt {
                Ok(bytes) if bytes == [1u8] => return,
                Ok(other) => last = format!("unexpected reply {other:?}"),
                Err(e) => last = e.to_string(),
            }
            if Instant::now() > deadline {
                let logs = docker(&["logs", "--tail", "40", &self.container_id]).unwrap_or_default();
                panic!("{test_name}: ClickHouse (container {}) not ready within {READY_TIMEOUT:?}; last error: {last}; docker logs:\n{logs}", self.container_id);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for ChServer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-fv", &self.container_id]).output();
        std::env::remove_var(&self.password_var);
    }
}

/// A scratch directory under `$HOME` (Colima shares only `$HOME`), removed on drop.
pub struct Stage {
    pub dir: PathBuf,
}

impl Stage {
    pub fn new(label: &str) -> Stage {
        let home = std::env::var("HOME").expect("HOME is set");
        let dir = Path::new(&home).join("clickhouse-stage").join(format!("{label}-{}", sanitize(&test_run_id())));
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("creating {dir:?}: {e}"));
        Stage { dir }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        // The parent goes too when this was the last stage (it is only ever a container for them).
        if let Some(parent) = self.dir.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

pub fn announce(test_name: &str, reason: &DockerGateReason) -> String {
    announce_gate_skip(test_name, reason)
}
