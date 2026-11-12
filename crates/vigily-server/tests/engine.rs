//! The `vigily-engine` process: its state file and lock, the idle shutdown of a background
//! engine, flushing on SIGTERM and the local token across restarts.

#![cfg(unix)]
#![recursion_limit = "256"]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vigily_api::local::{canonical_location, sha256_hex, EngineFile, EnginePaths};

const ENGINE: &str = env!("CARGO_BIN_EXE_vigily-engine");
const RUN: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a5b";

struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        Setup {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn paths(&self) -> EnginePaths {
        EnginePaths::new(
            &self.state(),
            &canonical_location(self.repo().to_str().unwrap()),
        )
    }

    /// The engine command on a free port, without background optimizing.
    fn command(&self, extra: &[&str]) -> Command {
        let mut c = Command::new(ENGINE);
        c.current_dir(self.dir.path())
            .env_remove("VIGILY_PORT")
            .env_remove("VIGILY_CONFIG")
            .env_remove("VIGILY_TOKEN_FILE")
            .env_remove("VIGILY_STATE_DIR")
            .env("VIGILY_LOG", "warn")
            .arg("--repo")
            .arg(self.repo())
            .arg("--state-dir")
            .arg(self.state())
            .args(["--port", "0", "--no-optimize"])
            .args(extra);
        c
    }

    fn spawn(&self) -> Child {
        self.command(&[])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }

    /// Waits for the engine file of process `pid`.
    fn ready(&self, pid: u32) -> EngineFile {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(f) = self.paths().read().filter(|f| f.pid == pid) {
                return f;
            }
            assert!(Instant::now() < deadline, "the engine did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn terminate(child: &mut Child) -> std::process::ExitStatus {
    // SAFETY: kill only sends a signal to the child we started.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the engine did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_state_file_names_the_engine_and_a_second_engine_refuses() {
    let s = Setup::new();
    let mut first = s.spawn();
    let file = s.ready(first.id());
    let paths = s.paths();
    assert!(file.url.starts_with("http://127.0.0.1:"));
    assert_ne!(file.url, "http://127.0.0.1:7600");
    assert_eq!(file.api_version, "v1");
    assert_eq!(file.mode, "local");
    assert_eq!(file.repo, canonical_location(s.repo().to_str().unwrap()));
    assert!(!file.auto_started);
    let token = file.token.clone().unwrap();
    assert_eq!(token.len(), 32);
    assert_eq!(std::fs::read_to_string(&paths.token).unwrap().trim(), token);
    assert_eq!(mode(&paths.json), 0o600);
    assert_eq!(mode(&paths.token), 0o600);
    assert_eq!(mode(&paths.dir), 0o700);
    assert!(paths.is_locked());

    let second: Output = s.command(&[]).output().unwrap();
    assert_eq!(second.status.code(), Some(3));
    let err = String::from_utf8_lossy(&second.stderr);
    assert!(
        err.contains(&format!("already served at {}", file.url)),
        "{err}"
    );

    assert!(terminate(&mut first).success());
    assert!(!paths.json.exists(), "the engine file is removed on exit");
    assert!(!paths.is_locked());
}

#[test]
fn a_stale_engine_file_is_replaced() {
    let s = Setup::new();
    let paths = s.paths();
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(
        &paths.json,
        r#"{"url":"http://127.0.0.1:9","pid":1,"version":"0","api_version":"v1","repo":"x","mode":"local","started_at":0}"#,
    )
    .unwrap();
    assert!(!paths.is_locked());
    let mut engine = s.spawn();
    let file = s.ready(engine.id());
    assert_ne!(file.url, "http://127.0.0.1:9");
    assert!(terminate(&mut engine).success());
}

#[test]
fn a_background_engine_stops_when_idle() {
    let s = Setup::new();
    let out = s
        .command(&["--detach", "--idle-timeout", "2s"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let paths = s.paths();
    let file = paths.read().unwrap();
    assert_eq!(file.url, url);
    assert!(file.auto_started);
    assert!(paths.is_locked());
    assert!(paths.log.exists());
    let deadline = Instant::now() + Duration::from_secs(60);
    while paths.json.exists() || paths.is_locked() {
        assert!(
            Instant::now() < deadline,
            "the idle engine did not stop: {}",
            std::fs::read_to_string(&paths.log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test]
async fn sigterm_flushes_what_was_ingested() {
    let s = Setup::new();
    let mut engine = s.spawn();
    let file = s.ready(engine.id());
    let http = reqwest::Client::new();
    let batch = json!({
        "writer": "0190a0b0c0d07e0f8a9b0c1d2e3f4a60",
        "seq": 1,
        "ops": [
            {"op": "create", "id": RUN, "name": "flushed", "created_at": 1_700_000_000_000_000i64},
            {"op": "points", "run_id": RUN, "name": "loss", "context": {},
             "steps": [0, 1, 2], "values": [0.5, 0.4, 0.3]},
        ]
    });
    let res = http
        .post(format!("{}/api/v1/ingest", file.url))
        .bearer_auth(file.token.as_deref().unwrap())
        .json(&batch)
        .send()
        .await
        .unwrap();
    let status = res.status();
    let body: Value = res.json().await.unwrap();
    assert!(status.is_success(), "{body}");
    assert!(terminate(&mut engine).success());

    let store = vigily_core::open_store(s.repo().to_str().unwrap())
        .await
        .unwrap();
    let runs = store.runs().await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, RUN);
    let series = store.series(RUN).await.unwrap();
    assert_eq!(series.len(), 1);
    let points = store
        .points(&[series[0].id], vigily_core::StepRange::default())
        .await
        .unwrap();
    assert_eq!(points[&series[0].id].steps.len(), 3);
}

#[test]
fn the_local_token_survives_restarts_and_moves_from_its_old_place() {
    let s = Setup::new();
    std::fs::create_dir_all(s.repo()).unwrap();
    // Where earlier releases kept it: tokens/<key>.token, keyed by the canonical path.
    let canonical = std::fs::canonicalize(s.repo()).unwrap();
    let old = s.state().join("tokens").join(format!(
        "{}.token",
        &sha256_hex(&canonical.to_string_lossy())[..16]
    ));
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "0123456789abcdef0123456789abcdef\n").unwrap();

    let mut engine = s.spawn();
    let first = s.ready(engine.id());
    assert_eq!(
        first.token.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    assert!(!old.exists(), "the old token file is moved");
    assert!(terminate(&mut engine).success());

    let mut engine = s.spawn();
    let second = s.ready(engine.id());
    assert_eq!(second.token, first.token);
    assert!(terminate(&mut engine).success());
}
