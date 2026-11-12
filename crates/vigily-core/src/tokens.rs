//! Access tokens, stored as records `auth/token/<id>` with the SHA-256 of the secret, never the
//! secret itself.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::records::RecordStore;

pub use vigily_api::auth::{Scope, TokenInfo};

const PREFIX: &str = "auth/token/";

/// A stored token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenRecord {
    /// SHA-256 of the whole token, in hex.
    pub hash: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created_at: i64,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub created_by: Option<String>,
}

impl TokenRecord {
    pub fn info(&self, id: &str) -> TokenInfo {
        TokenInfo {
            id: id.to_string(),
            name: self.name.clone(),
            scopes: self.scopes.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            expires_at: self.expires_at,
            created_by: self.created_by.clone(),
        }
    }

    /// Whether the token has expired at `now` (UTC microseconds).
    pub fn expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }
}

/// SHA-256 of a token, in hex.
pub fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn check_id(id: &str) -> Result<()> {
    if !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!("{id:?} is not a token id")))
    }
}

pub async fn put(rs: &dyn RecordStore, id: &str, token: &TokenRecord) -> Result<()> {
    check_id(id)?;
    let v = serde_json::to_value(token).map_err(|e| Error::Storage(e.to_string()))?;
    rs.put(vec![(format!("{PREFIX}{id}"), v)]).await?;
    rs.flush().await
}

pub async fn get(rs: &dyn RecordStore, id: &str) -> Result<Option<TokenRecord>> {
    check_id(id)?;
    match rs.get(&format!("{PREFIX}{id}")).await? {
        Some(v) => Ok(Some(
            serde_json::from_value(v).map_err(|e| Error::Storage(e.to_string()))?,
        )),
        None => Ok(None),
    }
}

/// Every stored token with its id, in id order.
pub async fn list(rs: &dyn RecordStore) -> Result<Vec<(String, TokenRecord)>> {
    rs.scan(PREFIX)
        .await?
        .into_iter()
        .map(|(k, v)| {
            let t = serde_json::from_value(v).map_err(|e| Error::Storage(e.to_string()))?;
            Ok((k[PREFIX.len()..].to_string(), t))
        })
        .collect()
}

/// Deletes a token; `false` when there was none.
pub async fn delete(rs: &dyn RecordStore, id: &str) -> Result<bool> {
    if get(rs, id).await?.is_none() {
        return Ok(false);
    }
    rs.delete(vec![format!("{PREFIX}{id}")]).await?;
    rs.flush().await?;
    Ok(true)
}
