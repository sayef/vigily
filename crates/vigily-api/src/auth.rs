//! Authentication: scopes, who a request is, browser sessions and access tokens.
//!
//! An engine runs in one of two modes. In `local` mode (one user on this machine) a token kept per
//! repository opens everything; the browser gets it once through `/?token=...` and keeps it in an
//! HttpOnly cookie. In `server` mode the operator's admin token (from the environment) opens
//! everything, and further tokens carry scopes: `read` (every read route), `ingest` (writing runs,
//! results, datasets and traces) and `admin` (everything, including edits in the web app, deletes,
//! storage administration and tokens).

use serde::{Deserialize, Serialize};

/// What a token may do. `admin` includes the other two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Ingest,
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Read, Scope::Ingest, Scope::Admin];

    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Ingest => "ingest",
            Scope::Admin => "admin",
        }
    }
}

/// `GET auth/me`: who the request is, and what it may do. Answered without a token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub authenticated: bool,
    /// `local`, `admin`, `token:<name>`, or `anonymous` on an engine without authentication.
    pub principal: Option<String>,
    pub scopes: Vec<Scope>,
    /// The engine's mode: `local` or `server`.
    pub mode: String,
    /// Whether the engine checks tokens at all (`false` with `--insecure-no-auth`).
    pub auth_required: bool,
}

/// `POST auth/session`: signs a browser in with a token; the answer sets the session cookie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRequest {
    pub token: String,
}

/// An access token as listed (never its secret).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    /// UTC microseconds.
    pub created_at: i64,
    /// UTC microseconds, updated at most every few minutes; `None` when never used.
    pub last_used_at: Option<i64>,
    /// UTC microseconds; `None` for a token that does not expire.
    pub expires_at: Option<i64>,
    /// Who created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// `GET auth/tokens`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub tokens: Vec<TokenInfo>,
}

/// `POST auth/tokens`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateToken {
    pub name: String,
    pub scopes: Vec<Scope>,
    /// UTC microseconds; the token never expires when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// The answer to `POST auth/tokens`: the secret, shown this once, and the token's details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreatedToken {
    pub token: String,
    #[serde(flatten)]
    pub info: TokenInfo,
}

/// `DELETE auth/tokens/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revoked {
    pub revoked: bool,
}
