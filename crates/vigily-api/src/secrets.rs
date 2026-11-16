//! Secrets (webhook URLs, signing keys, SMTP passwords) and the audit trail of configuration
//! changes.
//!
//! The engine keeps secret values encrypted in the repository's records and never returns them:
//! every answer describes a secret by its [`SecretState`] (where it comes from, a hint of its last
//! characters, who changed it and when). Values are written with `PUT secrets/{name}`; an empty
//! value removes the stored one.

use serde::{Deserialize, Serialize};

/// Where a secret comes from, and what can be shown about it without its value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecretState {
    /// `stored` (encrypted in the repository), `env:<NAME>` (read from the engine's environment
    /// when it is used) or `none`.
    pub source: String,
    /// `…` and the last four characters of a stored value of at least eight characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Who stored it (a principal such as `admin` or `token:ci`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
    /// When it was stored, UTC microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<i64>,
    /// A stored value that neither the current nor the previous key decrypts: store it again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undecryptable: bool,
}

impl SecretState {
    /// No value.
    pub fn none() -> Self {
        SecretState {
            source: "none".into(),
            ..Default::default()
        }
    }

    /// A value the engine reads from the environment variable `name` when it needs it.
    pub fn env(name: &str) -> Self {
        SecretState {
            source: format!("env:{name}"),
            ..Default::default()
        }
    }

    /// Whether a value is set, stored or from the environment.
    pub fn is_set(&self) -> bool {
        self.source != "none"
    }
}

/// A stored secret with its name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedSecret {
    pub name: String,
    #[serde(flatten)]
    pub state: SecretState,
}

/// `GET secrets`: the encryption key's status and every stored secret (never a value).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecretsStatus {
    /// Where the encryption key comes from: `env` (`VIGILY_SECRET_KEY`), `file` (a key the engine
    /// generated in its state directory, local mode only) or `none`.
    pub key: String,
    /// Whether `VIGILY_SECRET_KEY_PREVIOUS` (or, in local mode, an earlier generated key) is
    /// there to decrypt values stored under an older key.
    pub previous_key: bool,
    /// Whether secret values can be stored; without a key only `env:NAME` references work.
    pub can_store: bool,
    /// Why values cannot be stored and how to fix it, when `can_store` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub secrets: Vec<NamedSecret>,
}

/// `PUT secrets/{name}`: the value to store; an empty value removes the stored one.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SetSecret {
    #[serde(default)]
    pub value: String,
}

impl std::fmt::Debug for SetSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetSecret")
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A change to the engine's configuration (a secret, a channel, the routing rules): who made it
/// and what it touched, never a secret value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// UTC microseconds.
    pub at: i64,
    /// Who made it: `local`, `admin`, `token:<name>` or `anonymous`.
    pub principal: String,
    /// What happened, such as `secret.set`, `secret.removed`, `channel.created` or `rules.updated`.
    pub action: String,
    /// What it happened to: a secret's name, a channel's id, `rules`.
    pub target: String,
}

/// `GET audit`: configuration changes, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
}

/// `GET audit` parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditQuery {
    /// At most this many entries (100 when left out, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}
