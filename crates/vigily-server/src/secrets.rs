//! Secrets: webhook URLs, signing keys and SMTP passwords, stored encrypted in the repository's
//! records and never returned by the API.
//!
//! Each value is sealed with XChaCha20-Poly1305 under a key derived with BLAKE3
//! (`derive_key("vigily secrets v1", VIGILY_SECRET_KEY)`), a random 24-byte nonce per value, and
//! the secret's name as associated data, so a sealed value copied to another name does not open.
//! Records are `secret/<name>` → `{sealed, nonce, hint, changed_by, changed_at}` (hex for bytes).
//!
//! The key comes from `VIGILY_SECRET_KEY` (or the file `VIGILY_SECRET_KEY_FILE` names). A local
//! engine without it generates a key file (mode 0600) in its state directory, so storing secrets
//! needs no setup on a laptop; a server-mode engine has no key until one is set, and then refuses
//! to store values (channels can still read them from the environment with `env:NAME`).
//!
//! Rotation: start the engine with the new key in `VIGILY_SECRET_KEY` and the old one in
//! `VIGILY_SECRET_KEY_PREVIOUS`. At start every value sealed with the old key is sealed again with
//! the new one; values neither key opens are named in a warning and reported as `undecryptable`.
//!
//! | Method and path              | Scope | What it does |
//! |------------------------------|-------|--------------|
//! | `GET /api/v1/secrets`        | read  | The key's status and every stored secret, without values |
//! | `PUT /api/v1/secrets/{name}` | admin | Stores `{"value"}`; an empty value removes it |

use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use vigily_api::code;
use vigily_api::secrets::{NamedSecret, SecretState, SecretsStatus, SetSecret};
use vigily_core::{now_micros, RecordStore};

use crate::auth::{AuthContext, Scoped};
use crate::{ApiError, AppState};

/// The BLAKE3 key-derivation context of the sealing key.
pub const KEY_CONTEXT: &str = "vigily secrets v1";

/// The shortest key accepted from the environment.
pub const MIN_KEY: usize = 16;

/// The longest secret value accepted, in bytes.
pub const MAX_VALUE: usize = 8 * 1024;

const PREFIX: &str = "secret/";

/// What the API answers when a value cannot be stored for want of a key.
pub const NO_KEY: &str = "storing secret values needs an encryption key: set VIGILY_SECRET_KEY (or \
VIGILY_SECRET_KEY_FILE) to a long random value, such as the output of `openssl rand -hex 32`, and \
restart the engine. Without a key, reference an environment variable of the engine instead (env:NAME)";

/// Where the sealing key comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeySource {
    /// No key: values cannot be stored.
    #[default]
    None,
    /// `VIGILY_SECRET_KEY` or `VIGILY_SECRET_KEY_FILE`.
    Env,
    /// A key file the local engine generated in its state directory.
    File,
}

impl KeySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            KeySource::None => "none",
            KeySource::Env => "env",
            KeySource::File => "file",
        }
    }
}

/// The current key, the previous one (rotation) and where the current one comes from.
#[derive(Clone, Default)]
pub struct SecretKeys {
    pub current: Option<String>,
    pub previous: Option<String>,
    pub source: KeySource,
}

impl std::fmt::Debug for SecretKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKeys")
            .field("source", &self.source)
            .field("current", &self.current.as_ref().map(|_| "<redacted>"))
            .field("previous", &self.previous.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// `NAME` from the environment, or the contents of the file `NAME_FILE` names (Docker secrets),
/// trimmed. `Ok(None)` when neither is set.
pub fn env_secret(name: &str) -> Result<Option<String>, String> {
    if let Some(v) = std::env::var(name).ok().filter(|v| !v.trim().is_empty()) {
        return Ok(Some(v.trim().to_string()));
    }
    let Some(path) = std::env::var(format!("{name}_FILE"))
        .ok()
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    let v = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {name}_FILE {path}: {e}"))?;
    Ok(Some(v.trim().to_string()).filter(|v| !v.is_empty()))
}

impl SecretKeys {
    /// `VIGILY_SECRET_KEY` and `VIGILY_SECRET_KEY_PREVIOUS` (each also as `*_FILE`).
    pub fn from_env() -> Result<Self, String> {
        let current = env_secret("VIGILY_SECRET_KEY")?;
        let previous = env_secret("VIGILY_SECRET_KEY_PREVIOUS")?;
        for (name, key) in [
            ("VIGILY_SECRET_KEY", &current),
            ("VIGILY_SECRET_KEY_PREVIOUS", &previous),
        ] {
            if key.as_ref().is_some_and(|k| k.chars().count() < MIN_KEY) {
                return Err(format!(
                    "{name} is shorter than {MIN_KEY} characters; use a long random value, such as \
                     the output of `openssl rand -hex 32`"
                ));
            }
        }
        Ok(SecretKeys {
            source: if current.is_some() {
                KeySource::Env
            } else {
                KeySource::None
            },
            current,
            previous,
        })
    }

    /// For a local engine: without a key from the environment, the key in `file`, generated
    /// (mode 0600) when missing. With one, an existing key file still opens values sealed before
    /// the environment key was set (unless `VIGILY_SECRET_KEY_PREVIOUS` is given).
    pub fn with_local_file(mut self, file: &Path) -> std::io::Result<Self> {
        if self.current.is_none() {
            let key = crate::auth::load_or_create(file, || {
                format!("{}{}", crate::auth::new_secret(), crate::auth::new_secret())
            })?;
            self.current = Some(key);
            self.source = KeySource::File;
        } else if self.previous.is_none() {
            if let Ok(k) = std::fs::read_to_string(file) {
                self.previous = Some(k.trim().to_string()).filter(|k| !k.is_empty());
            }
        }
        Ok(self)
    }
}

fn cipher(key: &str) -> XChaCha20Poly1305 {
    let k = blake3::derive_key(KEY_CONTEXT, key.as_bytes());
    XChaCha20Poly1305::new((&k).into())
}

/// Encrypts `value` under `key`, bound to the secret's `name`. Returns the ciphertext and nonce.
pub fn seal(key: &str, name: &str, value: &str) -> (Vec<u8>, [u8; 24]) {
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).expect("the operating system's random number generator");
    let payload = Payload {
        msg: value.as_bytes(),
        aad: name.as_bytes(),
    };
    let sealed = cipher(key)
        .encrypt(&XNonce::from(nonce), payload)
        .expect("XChaCha20-Poly1305 encrypts any message that fits in memory");
    (sealed, nonce)
}

/// Decrypts what [`seal`] made for `name` under `key`; `None` for another key or name, or
/// tampered bytes.
pub fn open(key: &str, name: &str, sealed: &[u8], nonce: &[u8]) -> Option<String> {
    let payload = Payload {
        msg: sealed,
        aad: name.as_bytes(),
    };
    let nonce: [u8; 24] = nonce.try_into().ok()?;
    let plain = cipher(key).decrypt(&XNonce::from(nonce), payload).ok()?;
    String::from_utf8(plain).ok()
}

/// `…` and the last four characters, for values long enough that four characters give little
/// away.
pub fn hint(value: &str) -> Option<String> {
    let n = value.chars().count();
    (n >= 8).then(|| format!("…{}", value.chars().skip(n - 4).collect::<String>()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A stored secret.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SealedSecret {
    /// Ciphertext and tag, hex.
    pub sealed: String,
    /// The 24-byte nonce, hex.
    pub nonce: String,
    #[serde(default)]
    pub hint: Option<String>,
    pub changed_by: String,
    /// UTC microseconds.
    pub changed_at: i64,
}

impl SealedSecret {
    fn new(key: &str, name: &str, value: &str, by: &str, at: i64) -> Self {
        let (sealed, nonce) = seal(key, name, value);
        SealedSecret {
            sealed: hex(&sealed),
            nonce: hex(&nonce),
            hint: hint(value),
            changed_by: by.to_string(),
            changed_at: at,
        }
    }

    fn open(&self, key: &str, name: &str) -> Option<String> {
        open(key, name, &unhex(&self.sealed)?, &unhex(&self.nonce)?)
    }
}

/// Why a secret could not be stored or read.
#[derive(Debug)]
pub enum SecretError {
    /// No key to seal with.
    NoKey,
    /// A bad name or value.
    Invalid(String),
    /// The stored value does not open with the current or previous key.
    Undecryptable(String),
    Storage(vigily_core::Error),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::NoKey => f.write_str(NO_KEY),
            SecretError::Invalid(m) => f.write_str(m),
            SecretError::Undecryptable(name) => write!(
                f,
                "the stored secret {name} cannot be decrypted with VIGILY_SECRET_KEY; set \
                 VIGILY_SECRET_KEY_PREVIOUS to the key it was stored with, or store it again"
            ),
            SecretError::Storage(e) => write!(f, "{e}"),
        }
    }
}

impl From<vigily_core::Error> for SecretError {
    fn from(e: vigily_core::Error) -> Self {
        SecretError::Storage(e)
    }
}

impl From<SecretError> for ApiError {
    fn from(e: SecretError) -> Self {
        match e {
            SecretError::NoKey => {
                ApiError::new(StatusCode::CONFLICT, NO_KEY).with_code(code::SECRET_KEY_MISSING)
            }
            SecretError::Invalid(m) => ApiError::new(StatusCode::BAD_REQUEST, m),
            e @ SecretError::Undecryptable(_) => ApiError::new(StatusCode::CONFLICT, e.to_string()),
            SecretError::Storage(e) => e.into(),
        }
    }
}

/// Checks a secret's name: 1 to 128 characters of `A-Z a-z 0-9 . _ -`.
pub fn check_name(name: &str) -> Result<(), SecretError> {
    if !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        Ok(())
    } else {
        Err(SecretError::Invalid(format!(
            "{name:?} is not a secret name: use 1 to 128 letters, digits, dots, dashes or underscores"
        )))
    }
}

/// The engine's secrets: its keys; the values live in the records.
pub struct Secrets {
    keys: RwLock<SecretKeys>,
}

impl Secrets {
    pub fn new(keys: SecretKeys) -> Self {
        Secrets {
            keys: RwLock::new(keys),
        }
    }

    pub fn can_store(&self) -> bool {
        self.keys.read().current.is_some()
    }

    /// The key's status and every stored secret.
    pub async fn status(&self, rs: &dyn RecordStore) -> Result<SecretsStatus, SecretError> {
        let keys = self.keys.read().clone();
        Ok(SecretsStatus {
            key: keys.source.as_str().to_string(),
            previous_key: keys.previous.is_some(),
            can_store: keys.current.is_some(),
            message: keys.current.is_none().then(|| NO_KEY.to_string()),
            secrets: self.list(rs).await?,
        })
    }

    async fn stored(
        &self,
        rs: &dyn RecordStore,
        name: &str,
    ) -> Result<Option<SealedSecret>, SecretError> {
        Ok(match rs.get(&format!("{PREFIX}{name}")).await? {
            Some(v) => Some(
                serde_json::from_value(v)
                    .map_err(|e| vigily_core::Error::Storage(format!("secret {name}: {e}")))?,
            ),
            None => None,
        })
    }

    /// The value under the current key, or the previous one.
    fn open_with_keys(&self, name: &str, s: &SealedSecret) -> Option<String> {
        let keys = self.keys.read().clone();
        [keys.current, keys.previous]
            .into_iter()
            .flatten()
            .find_map(|k| s.open(&k, name))
    }

    fn state_of(&self, name: &str, s: &SealedSecret) -> SecretState {
        SecretState {
            source: "stored".into(),
            hint: s.hint.clone(),
            changed_by: Some(s.changed_by.clone()),
            changed_at: Some(s.changed_at),
            undecryptable: self.open_with_keys(name, s).is_none(),
        }
    }

    /// A secret's state; `none` when nothing is stored under `name`.
    pub async fn state(
        &self,
        rs: &dyn RecordStore,
        name: &str,
    ) -> Result<SecretState, SecretError> {
        check_name(name)?;
        Ok(match self.stored(rs, name).await? {
            Some(s) => self.state_of(name, &s),
            None => SecretState::none(),
        })
    }

    /// Every stored secret, in name order.
    pub async fn list(&self, rs: &dyn RecordStore) -> Result<Vec<NamedSecret>, SecretError> {
        let mut out = Vec::new();
        for (k, v) in rs.scan(PREFIX).await? {
            let name = k[PREFIX.len()..].to_string();
            let state = match serde_json::from_value::<SealedSecret>(v) {
                Ok(s) => self.state_of(&name, &s),
                Err(_) => SecretState {
                    source: "stored".into(),
                    undecryptable: true,
                    ..Default::default()
                },
            };
            out.push(NamedSecret { name, state });
        }
        Ok(out)
    }

    /// Stores `value` under `name`, sealed with the current key; an empty value removes it.
    /// Returns the new state. The caller records the change in the audit trail.
    pub async fn set(
        &self,
        rs: &dyn RecordStore,
        name: &str,
        value: &str,
        by: &str,
    ) -> Result<SecretState, SecretError> {
        check_name(name)?;
        let value = value.trim();
        let key = format!("{PREFIX}{name}");
        if value.is_empty() {
            rs.delete(vec![key]).await?;
            rs.flush().await?;
            return Ok(SecretState::none());
        }
        if value.len() > MAX_VALUE {
            return Err(SecretError::Invalid(format!(
                "a secret value is at most {MAX_VALUE} bytes"
            )));
        }
        let Some(current) = self.keys.read().current.clone() else {
            return Err(SecretError::NoKey);
        };
        let row = SealedSecret::new(&current, name, value, by, now_micros());
        let state = self.state_of(name, &row);
        let v =
            serde_json::to_value(&row).map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
        rs.put(vec![(key, v)]).await?;
        rs.flush().await?;
        Ok(state)
    }

    /// The value stored under `name`; `None` when nothing is stored.
    pub async fn reveal(
        &self,
        rs: &dyn RecordStore,
        name: &str,
    ) -> Result<Option<String>, SecretError> {
        check_name(name)?;
        match self.stored(rs, name).await? {
            Some(s) => self
                .open_with_keys(name, &s)
                .map(Some)
                .ok_or_else(|| SecretError::Undecryptable(name.to_string())),
            None => Ok(None),
        }
    }

    /// Seals every value the previous key opens again with the current key. Returns the names of
    /// the values no key opens.
    pub async fn reseal(&self, rs: &dyn RecordStore) -> Result<Vec<String>, SecretError> {
        let keys = self.keys.read().clone();
        let mut again = Vec::new();
        let mut unreadable = Vec::new();
        for (k, v) in rs.scan(PREFIX).await? {
            let name = &k[PREFIX.len()..];
            let Ok(row) = serde_json::from_value::<SealedSecret>(v) else {
                unreadable.push(name.to_string());
                continue;
            };
            let Some(current) = &keys.current else {
                unreadable.push(name.to_string());
                continue;
            };
            if row.open(current, name).is_some() {
                continue;
            }
            match keys.previous.as_ref().and_then(|p| row.open(p, name)) {
                Some(plain) => {
                    let mut new =
                        SealedSecret::new(current, name, &plain, &row.changed_by, row.changed_at);
                    new.hint = row.hint.clone();
                    let v = serde_json::to_value(&new)
                        .map_err(|e| vigily_core::Error::Storage(e.to_string()))?;
                    again.push((k.clone(), v));
                }
                None => unreadable.push(name.to_string()),
            }
        }
        if !again.is_empty() {
            let n = again.len();
            rs.put(again).await?;
            rs.flush().await?;
            tracing::info!(count = n, "re-sealed stored secrets with the current key");
        }
        Ok(unreadable)
    }
}

/// At start: re-seals values stored under the previous key and warns about values no key opens.
pub async fn startup(state: &AppState) {
    match state.secrets.reseal(&*state.records).await {
        Ok(names) if names.is_empty() => {}
        Ok(names) if !state.secrets.can_store() => tracing::warn!(
            count = names.len(),
            names = %names.join(", "),
            "stored secrets cannot be decrypted: this engine has no VIGILY_SECRET_KEY"
        ),
        Ok(names) => tracing::warn!(
            count = names.len(),
            names = %names.join(", "),
            "stored secrets cannot be decrypted with VIGILY_SECRET_KEY; set \
             VIGILY_SECRET_KEY_PREVIOUS to the key they were stored with, or store them again"
        ),
        Err(e) => tracing::warn!(error = %e, "could not check the stored secrets"),
    }
}

/// Reads a JSON body that may hold a secret value. A parse error names the line and column only,
/// never the text around it.
pub(crate) fn secret_body<T: serde::de::DeserializeOwned>(
    bytes: &Bytes,
    shape: &str,
) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "send {shape}: the body is not valid for this route (line {}, column {})",
                e.line(),
                e.column()
            ),
        )
    })
}

/// The secret routes, relative to the API root.
pub fn routes() -> Scoped {
    Scoped::default()
        .read(Router::new().route("/secrets", get(list)))
        .admin(Router::new().route("/secrets/{name}", put(set)))
}

async fn list(State(s): State<Arc<AppState>>) -> Result<Json<SecretsStatus>, ApiError> {
    Ok(Json(s.secrets.status(&*s.records).await?))
}

async fn set(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    UrlPath(name): UrlPath<String>,
    bytes: Bytes,
) -> Result<Json<NamedSecret>, ApiError> {
    let req: SetSecret = secret_body(&bytes, "{\"value\": \"...\"}")?;
    check_name(&name)?;
    let removed = req.value.trim().is_empty();
    let state = s
        .secrets
        .set(&*s.records, &name, &req.value, &ctx.principal)
        .await?;
    let action = if removed {
        "secret.removed"
    } else {
        "secret.set"
    };
    crate::audit::record(&*s.records, &ctx.principal, action, &name).await;
    Ok(Json(NamedSecret { name, state }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vigily_core::MemoryRecords;

    /// A synthetic key, made fresh for each test.
    fn key() -> String {
        format!(
            "test-key-{}{}",
            crate::auth::new_secret(),
            crate::auth::new_secret()
        )
    }

    fn keys(current: Option<&str>, previous: Option<&str>) -> SecretKeys {
        SecretKeys {
            current: current.map(str::to_string),
            previous: previous.map(str::to_string),
            source: if current.is_some() {
                KeySource::Env
            } else {
                KeySource::None
            },
        }
    }

    #[test]
    fn seal_and_open_round_trip() {
        let k = key();
        let (sealed, nonce) = seal(&k, "slack", "synthetic-value");
        assert_eq!(
            open(&k, "slack", &sealed, &nonce).as_deref(),
            Some("synthetic-value")
        );
        // A fresh nonce per value: sealing twice gives different bytes.
        let (again, nonce2) = seal(&k, "slack", "synthetic-value");
        assert_ne!(nonce, nonce2);
        assert_ne!(sealed, again);
    }

    #[test]
    fn a_wrong_key_does_not_open() {
        let (sealed, nonce) = seal(&key(), "slack", "synthetic-value");
        assert_eq!(open(&key(), "slack", &sealed, &nonce), None);
    }

    #[test]
    fn the_name_is_bound_to_the_value() {
        let k = key();
        let (sealed, nonce) = seal(&k, "slack", "synthetic-value");
        assert_eq!(open(&k, "teams", &sealed, &nonce), None);
        let mut tampered = sealed.clone();
        tampered[0] ^= 1;
        assert_eq!(open(&k, "slack", &tampered, &nonce), None);
    }

    #[test]
    fn hints_show_four_characters_of_long_values_only() {
        assert_eq!(hint("abcdefgh-wxyz").as_deref(), Some("…wxyz"));
        assert_eq!(hint("short"), None);
    }

    #[tokio::test]
    async fn moving_a_stored_value_to_another_name_makes_it_undecryptable() {
        let rs = MemoryRecords::new();
        let k = key();
        let s = Secrets::new(keys(Some(&k), None));
        s.set(&rs, "a", "synthetic-value-a", "admin").await.unwrap();
        let v = rs.get("secret/a").await.unwrap().unwrap();
        rs.put(vec![("secret/b".into(), v)]).await.unwrap();
        assert!(s.state(&rs, "b").await.unwrap().undecryptable);
        assert!(matches!(
            s.reveal(&rs, "b").await,
            Err(SecretError::Undecryptable(_))
        ));
        assert_eq!(
            s.reveal(&rs, "a").await.unwrap().as_deref(),
            Some("synthetic-value-a")
        );
    }

    #[tokio::test]
    async fn rotation_reseals_with_the_new_key_and_reports_the_rest() {
        let rs = MemoryRecords::new();
        let (old, new, lost) = (key(), key(), key());
        let before = Secrets::new(keys(Some(&old), None));
        before
            .set(&rs, "kept", "synthetic-value-1", "admin")
            .await
            .unwrap();
        Secrets::new(keys(Some(&lost), None))
            .set(&rs, "lost", "synthetic-value-2", "admin")
            .await
            .unwrap();
        let after = Secrets::new(keys(Some(&new), Some(&old)));
        assert_eq!(after.reseal(&rs).await.unwrap(), vec!["lost".to_string()]);
        // The new key alone now opens the re-sealed value; who stored it is kept.
        let only_new = Secrets::new(keys(Some(&new), None));
        assert_eq!(
            only_new.reveal(&rs, "kept").await.unwrap().as_deref(),
            Some("synthetic-value-1")
        );
        let state = only_new.state(&rs, "kept").await.unwrap();
        assert_eq!(state.changed_by.as_deref(), Some("admin"));
        assert!(!state.undecryptable);
        assert!(only_new.state(&rs, "lost").await.unwrap().undecryptable);
    }

    #[tokio::test]
    async fn without_a_key_values_are_refused_and_stored_ones_reported() {
        let rs = MemoryRecords::new();
        Secrets::new(keys(Some(&key()), None))
            .set(&rs, "a", "synthetic-value", "admin")
            .await
            .unwrap();
        let none = Secrets::new(keys(None, None));
        assert!(matches!(
            none.set(&rs, "b", "synthetic-value", "admin").await,
            Err(SecretError::NoKey)
        ));
        assert_eq!(none.reseal(&rs).await.unwrap(), vec!["a".to_string()]);
        // Removing needs no key.
        assert!(!none.set(&rs, "a", "", "admin").await.unwrap().is_set());
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("channel.0190ab.webhook_url").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name(&"x".repeat(129)).is_err());
    }
}
