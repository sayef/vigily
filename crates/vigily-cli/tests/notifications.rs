//! `vigily channels`, `vigily rules` and `vigily secrets` against an engine in this process, with
//! a webhook receiver in this process too. Secret values are synthetic and must never appear in
//! any output.

#![recursion_limit = "256"]

use std::io::Write as _;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vigily_api::client::Client;
use vigily_cli::notify;
use vigily_cli::{Cli, Command};
use vigily_core::{MemoryRecords, MemoryStore};
use vigily_server::config::Config;
use vigily_server::delivery::DeliveryOptions;
use vigily_server::secrets::{KeySource, SecretKeys};
use vigily_server::{serve, AppState, AuthSetup, EngineOptions, ServeOptions};

const VIGILY: &str = env!("CARGO_BIN_EXE_vigily");

/// A synthetic value no real service would accept, unique per call.
fn synthetic(prefix: &str) -> String {
    format!("{prefix}-{}", vigily_server::auth::new_secret())
}

/// An engine in local mode with an encryption key (or none), and its URL and token.
async fn engine(dir: &std::path::Path, key: Option<String>) -> (String, String) {
    let state = AppState::with_options(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryRecords::new()),
        Config::default(),
        EngineOptions {
            secrets: SecretKeys {
                source: if key.is_some() {
                    KeySource::Env
                } else {
                    KeySource::None
                },
                current: key,
                previous: None,
            },
            delivery: DeliveryOptions {
                attempts: 2,
                base_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(50),
                timeout: Duration::from_secs(5),
            },
            ..Default::default()
        },
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = ServeOptions {
        addr: "127.0.0.1:0".parse().unwrap(),
        local_only: true,
        optimize: false,
        optimize_every: Duration::from_secs(3600),
        optimize_at_start: false,
        auth: AuthSetup::Local {
            token_file: dir.join("token"),
        },
        port_fallback: false,
        stop: None,
    };
    tokio::spawn(serve(state, opts, move |r| {
        let _ = tx.send((r.url.clone(), r.token.clone().unwrap()));
    }));
    rx.await.unwrap()
}

/// What the receiver got: path and headers, one entry per request.
type Seen = Arc<Mutex<Vec<String>>>;

/// An HTTP receiver on 127.0.0.1: 500 for paths under /fail, 200 otherwise.
async fn receiver() -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Head, then as much body as content-length says.
                let head_end = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + len {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let fail = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .starts_with("/fail");
                log.lock().unwrap().push(head);
                let answer: &[u8] = if fail {
                    b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnope"
                } else {
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                };
                let _ = s.write_all(answer).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn parse(args: &[&str]) -> Command {
    let mut v = vec!["vigily"];
    v.extend_from_slice(args);
    Cli::try_parse_from(v).unwrap().command
}

/// Runs a `vigily channels|rules|secrets` command line; secret values come from `values`.
async fn cmd(c: &Client, args: &[&str], values: &[&str]) -> (Result<ExitCode>, String) {
    let mut out = Vec::new();
    let mut given: Vec<String> = values.iter().rev().map(|v| v.to_string()).collect();
    let mut asked = Vec::new();
    let mut read = |what: &str| -> Result<String> {
        asked.push(what.to_string());
        given
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no value for {what}"))
    };
    let r = match parse(args) {
        Command::Channels { action } => notify::channels(c, &mut out, &action, &mut read).await,
        Command::Rules { action } => notify::rules(c, &mut out, &action).await,
        Command::Secrets { action } => notify::secrets(c, &mut out, &action).await,
        _ => unreachable!(),
    };
    (r, String::from_utf8_lossy(&out).to_string())
}

async fn ok(c: &Client, args: &[&str], values: &[&str]) -> String {
    let (r, out) = cmd(c, args, values).await;
    assert_eq!(r.unwrap(), ExitCode::SUCCESS, "{args:?}: {out}");
    out
}

async fn err(c: &Client, args: &[&str], values: &[&str]) -> String {
    let (r, out) = cmd(c, args, values).await;
    let e = r.expect_err(&format!("{args:?} should fail: {out}"));
    format!("{e:#}")
}

#[tokio::test]
async fn channels_are_added_listed_tested_edited_and_removed_without_showing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir.path(), Some(synthetic("key"))).await;
    let c = Client::new(&url, Some(token));
    let (rx, seen) = receiver().await;

    let out = ok(&c, &["channels", "list"], &[]).await;
    assert!(out.starts_with("No channels."), "{out}");

    // The required secret is asked for; nothing else is.
    let hook = format!("{rx}/slack/T000/B000/{}", synthetic("path"));
    let out = ok(
        &c,
        &[
            "channels",
            "add",
            "ml-alerts",
            "--kind",
            "slack",
            "--default",
        ],
        &[&hook],
    )
    .await;
    assert!(out.contains("Added channel ml-alerts (slack"), "{out}");
    assert!(out.contains("webhook_url: stored …"), "{out}");
    assert!(
        !out.contains(&hook) && !out.contains("/slack/T000"),
        "{out}"
    );

    // A webhook with its URL stored and its signing key from the engine's environment.
    let out = ok(
        &c,
        &[
            "channels",
            "add",
            "bridge",
            "--kind",
            "webhook",
            "--env",
            "signing_key=QA_UNSET_SIGNING_KEY",
            "--burst",
            "3",
        ],
        &[&format!("{rx}/fail/hook")],
    )
    .await;
    assert!(
        out.contains("signing_key: env QA_UNSET_SIGNING_KEY"),
        "{out}"
    );

    let out = ok(&c, &["channels", "list"], &[]).await;
    assert!(
        out.contains("ml-alerts") && out.contains("default"),
        "{out}"
    );
    assert!(out.contains("bridge") && out.contains("by rule"), "{out}");
    assert!(!out.contains(&hook), "{out}");

    // Test sends: by name, then by id; a failure exits 1 and names the reason verbatim.
    let out = ok(&c, &["channels", "test", "ML-ALERTS"], &[]).await;
    assert!(
        out.starts_with("Sent a test alert to ml-alerts (200)"),
        "{out}"
    );
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|h| h.contains("/slack/T000")));
    let (r, out) = cmd(&c, &["channels", "test", "bridge"], &[]).await;
    assert_eq!(r.unwrap(), ExitCode::FAILURE);
    assert!(out.contains("failed"), "{out}");
    assert!(out.contains("QA_UNSET_SIGNING_KEY"), "{out}");

    let out = ok(&c, &["channels", "list"], &[]).await;
    assert!(out.contains("sent 200 (test)"), "{out}");

    // Edit: settings merge, a secret is replaced, an optional one removed.
    let id = c
        .channels()
        .await
        .unwrap()
        .channels
        .into_iter()
        .find(|ch| ch.name == "bridge")
        .unwrap()
        .id;
    let out = ok(
        &c,
        &[
            "channels",
            "edit",
            &id,
            "--name",
            "incident-bridge",
            "--per-minute",
            "10",
            "--secret",
            "url",
            "--remove-secret",
            "signing_key",
        ],
        &[&format!("{rx}/hooks/vigily")],
    )
    .await;
    assert!(out.contains("Saved channel incident-bridge"), "{out}");
    assert!(out.contains("signing_key: not set"), "{out}");
    let ch = c.channel(&id).await.unwrap();
    assert_eq!(
        (ch.settings.burst, ch.settings.per_minute),
        (Some(3), Some(10))
    );
    let out = ok(&c, &["channels", "test", "incident-bridge"], &[]).await;
    assert!(out.contains("(200)"), "{out}");

    // Mistakes are caught before anything is sent.
    let e = err(&c, &["channels", "edit", "incident-bridge"], &[]).await;
    assert!(e.contains("nothing to change"), "{e}");
    let e = err(
        &c,
        &[
            "channels",
            "edit",
            "ml-alerts",
            "--remove-secret",
            "webhook_url",
        ],
        &[],
    )
    .await;
    assert!(e.contains("required"), "{e}");
    let e = err(
        &c,
        &[
            "channels", "add", "x", "--kind", "slack", "--secret", "password",
        ],
        &[],
    )
    .await;
    assert!(e.contains("no secret \"password\""), "{e}");
    let e = err(&c, &["channels", "add", "x", "--kind", "pager"], &[]).await;
    assert!(e.contains("unknown kind"), "{e}");
    let e = err(&c, &["channels", "test", "nope"], &[]).await;
    assert!(e.contains("no channel with the id or name"), "{e}");
    let e = err(
        &c,
        &["channels", "add", "ML-alerts", "--kind", "discord"],
        &[&format!("{rx}/x")],
    )
    .await;
    assert!(e.contains("exists already"), "{e}");

    let out = ok(&c, &["channels", "remove", "incident-bridge"], &[]).await;
    assert!(out.starts_with("Removed channel incident-bridge"), "{out}");
    assert_eq!(c.channels().await.unwrap().channels.len(), 1);
}

#[tokio::test]
async fn rules_round_trip_through_toml_and_json_with_the_version_check() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir.path(), Some(synthetic("key"))).await;
    let c = Client::new(&url, Some(token));
    for (name, kind) in [("ml-alerts", "slack"), ("pager", "discord")] {
        ok(
            &c,
            &[
                "channels",
                "add",
                name,
                "--kind",
                kind,
                "--env",
                "webhook_url=HOOK",
                "--default",
            ],
            &[],
        )
        .await;
    }
    let out = ok(&c, &["rules", "list"], &[]).await;
    assert!(out.starts_with("No routing rules."), "{out}");
    assert!(out.contains("default: ml-alerts, pager"), "{out}");

    let rules = r#"
[[rules]]
name = "Critical drift"
channels = ["pager"]
stop = true
[rules.match]
kinds = ["behaviour_drift", "data_drift"]
severities = ["critical"]
experiment = "intents*"

[[rules]]
name = "Silence the sandbox"
STOP
[rules.match]
experiment = "sandbox-*"
"#;
    // A rule without channels must stop: the engine says which one.
    let file = dir.path().join("rules.toml");
    std::fs::write(&file, rules.replace("STOP\n", "")).unwrap();
    let e = err(&c, &["rules", "set", file.to_str().unwrap()], &[]).await;
    assert!(e.contains("rule 2"), "{e}");
    std::fs::write(&file, rules.replace("STOP", "stop = true")).unwrap();
    let out = ok(&c, &["rules", "set", file.to_str().unwrap()], &[]).await;
    assert_eq!(out, "Saved 2 rules (version 1).\n");

    let out = ok(&c, &["rules", "list"], &[]).await;
    assert!(out.contains("Critical drift"), "{out}");
    assert!(
        out.contains("behaviour_drift|data_drift, critical, experiment intents*"),
        "{out}"
    );
    assert!(out.contains("Version 1, saved by local"), "{out}");

    // show writes a file set takes back unchanged (channel names, the version).
    let shown = ok(&c, &["rules", "show"], &[]).await;
    assert!(shown.contains("version = 1"), "{shown}");
    assert!(shown.contains("channels = [\"pager\"]"), "{shown}");
    let again = dir.path().join("again.toml");
    std::fs::write(&again, &shown).unwrap();
    let out = ok(&c, &["rules", "set", again.to_str().unwrap()], &[]).await;
    assert_eq!(out, "Saved 2 rules (version 2).\n");

    // The same file again is stale now: nothing is saved.
    let e = err(&c, &["rules", "set", again.to_str().unwrap()], &[]).await;
    assert!(e.contains("changed since version 1"), "{e}");
    assert!(e.contains("vigily rules show"), "{e}");
    assert_eq!(c.rules().await.unwrap().version, 2);

    // JSON works the same way; without a version, the current one is used.
    let json = ok(&c, &["rules", "show", "--json"], &[]).await;
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rules"][0]["channels"][0], "pager");
    v.as_object_mut().unwrap().remove("version");
    v["rules"].as_array_mut().unwrap().pop();
    let jf = dir.path().join("rules.json");
    std::fs::write(&jf, v.to_string()).unwrap();
    let out = ok(&c, &["rules", "set", jf.to_str().unwrap()], &[]).await;
    assert_eq!(out, "Saved 1 rule (version 3).\n");

    // Unknown channels and misspelt keys are refused before sending.
    std::fs::write(&jf, r#"{"rules": [{"channels": ["nobody"]}]}"#).unwrap();
    let e = err(&c, &["rules", "set", jf.to_str().unwrap()], &[]).await;
    assert!(e.contains("rule 1: no channel"), "{e}");
    std::fs::write(&jf, r#"{"rules": [{"channel": ["pager"]}]}"#).unwrap();
    let e = err(&c, &["rules", "set", jf.to_str().unwrap()], &[]).await;
    assert!(e.contains("unknown field `channel`"), "{e}");
}

#[tokio::test]
async fn secrets_status_shows_states_and_how_to_store_without_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir.path(), Some(synthetic("key"))).await;
    let c = Client::new(&url, Some(token));
    let value = format!("https://hooks.example.com/{}", synthetic("v"));
    ok(
        &c,
        &["channels", "add", "ml-alerts", "--kind", "slack"],
        &[&value],
    )
    .await;
    ok(
        &c,
        &[
            "channels",
            "add",
            "mail",
            "--kind",
            "email",
            "--host",
            "smtp.example.com",
            "--from",
            "vigily <vigily@example.com>",
            "--to",
            "oncall@example.com",
            "--env",
            "password=SMTP_PASSWORD",
        ],
        &[],
    )
    .await;
    let out = ok(&c, &["secrets", "status"], &[]).await;
    assert!(
        out.starts_with("Encryption key: VIGILY_SECRET_KEY\n"),
        "{out}"
    );
    assert!(out.contains("Previous key: not set"), "{out}");
    assert!(out.contains("ml-alerts  webhook_url  stored …"), "{out}");
    assert!(
        out.contains("mail       password     env SMTP_PASSWORD"),
        "{out}"
    );
    assert!(out.contains("local, "), "{out}");
    assert!(!out.contains(&value), "{out}");

    // Without a key, values are refused with the engine's explanation; env references work.
    let dir2 = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir2.path(), None).await;
    let c = Client::new(&url, Some(token));
    let out = ok(&c, &["secrets", "status"], &[]).await;
    assert!(out.contains("Encryption key: none"), "{out}");
    assert!(out.contains("VIGILY_SECRET_KEY"), "{out}");
    let e = err(
        &c,
        &["channels", "add", "ml-alerts", "--kind", "slack"],
        &[&value],
    )
    .await;
    assert!(e.contains("VIGILY_SECRET_KEY"), "{e}");
    ok(
        &c,
        &[
            "channels",
            "add",
            "ml-alerts",
            "--kind",
            "slack",
            "--env",
            "webhook_url=SLACK_WEBHOOK_URL",
        ],
        &[],
    )
    .await;
}

#[tokio::test]
async fn the_binary_reads_secret_values_from_stdin_and_never_from_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir.path(), Some(synthetic("key"))).await;
    let (rx, seen) = receiver().await;
    let value = format!("{rx}/hooks/{}", synthetic("p"));
    let key = synthetic("signing");
    let run = |args: Vec<String>, input: String| {
        let (url, token) = (url.clone(), token.clone());
        tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(VIGILY)
                .args(["--url", &url])
                .args(&args)
                .env("VIGILY_TOKEN", &token)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        })
    };
    let o = run(
        [
            "channels",
            "add",
            "bridge",
            "--kind",
            "webhook",
            "--secret",
            "signing_key",
            "--stdin",
        ]
        .map(String::from)
        .to_vec(),
        format!("{value}\n{key}\n"),
    )
    .await
    .unwrap();
    let (out, errs) = (
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    );
    assert!(o.status.success(), "{out}{errs}");
    assert!(out.contains("url: stored …") && out.contains("signing_key: stored …"));
    assert!(!out.contains(&value) && !out.contains(&key) && !errs.contains(&key));

    // The request is signed with the key read from stdin.
    let o = run(
        ["channels", "test", "bridge"].map(String::from).to_vec(),
        String::new(),
    )
    .await
    .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(seen.lock().unwrap().iter().any(|h| h
        .to_ascii_lowercase()
        .contains("x-vigily-signature: sha256=")));

    // Running out of input stops before anything is stored.
    let o = run(
        ["channels", "add", "other", "--kind", "slack", "--stdin"]
            .map(String::from)
            .to_vec(),
        String::new(),
    )
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("standard input ended"));

    // There is no flag that takes a value on the command line.
    let o = run(
        ["channels", "add", "x", "--kind", "slack", "--value", "v"]
            .map(String::from)
            .to_vec(),
        String::new(),
    )
    .await
    .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
