//! Local engines: where an engine on this machine writes down how to reach it.
//!
//! Each repository has at most one engine on a machine. Its files live in the state directory
//! (`$VIGILY_STATE_DIR`, else `~/Library/Caches/vigily` on macOS and `~/.cache/vigily` elsewhere),
//! under `engines/`, named by the repository's [key](repo_key):
//!
//! | File          | What it holds |
//! |---------------|---------------|
//! | `<key>.json`  | [`EngineFile`]: address, process, version and token of the running engine (0600; removed on exit) |
//! | `<key>.lock`  | Locked by the engine for its whole life; a second engine on the repository refuses to start |
//! | `<key>.token` | The repository's local token, kept across restarts (0600) |
//! | `<key>.log`   | The output of an engine started in the background |
//!
//! An `<key>.json` whose lock nobody holds is left over from an engine that did not exit cleanly,
//! and is ignored.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The engine's port unless told otherwise.
pub const DEFAULT_PORT: u16 = 7600;

/// What a running engine wrote down about itself in `engines/<key>.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineFile {
    /// Where it answers: `http://127.0.0.1:7600`.
    pub url: String,
    pub pid: u32,
    /// The engine's version.
    pub version: String,
    /// The API version it serves (`v1`).
    pub api_version: String,
    /// The repository it serves, as its canonical location.
    pub repo: String,
    /// The local token, which opens the whole API; `None` in server mode or without sign-in.
    #[serde(default)]
    pub token: Option<String>,
    /// `local` or `server`.
    pub mode: String,
    /// When it started (UTC microseconds).
    pub started_at: i64,
    /// Started in the background (by a client, or with `--detach`); such an engine stops after a
    /// while without use.
    #[serde(default)]
    pub auto_started: bool,
}

/// The state directory: `$VIGILY_STATE_DIR`, else the user's cache directory with `vigily`.
/// `None` without a home directory.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("VIGILY_STATE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))?,
    );
    Some(if cfg!(target_os = "macos") {
        home.join("Library").join("Caches").join("vigily")
    } else {
        home.join(".cache").join("vigily")
    })
}

/// A repository's location in one spelling: object-store URIs as given (without a trailing `/`),
/// directories as absolute paths with symbolic links resolved. A directory that does not exist
/// yet gets the spelling it will have once created, so a client and the engine that creates it
/// agree on the key.
pub fn canonical_location(repo: &str) -> String {
    if repo.contains("://") {
        return repo.trim_end_matches('/').to_string();
    }
    let path = Path::new(repo);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    if let Ok(c) = std::fs::canonicalize(&absolute) {
        return c.to_string_lossy().to_string();
    }
    // Resolve the deepest part that exists and append the rest, without `.` and `..`.
    let mut lexical = PathBuf::new();
    for c in absolute.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut rest = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(c) = std::fs::canonicalize(base) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out.to_string_lossy().to_string();
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return lexical.to_string_lossy().to_string(),
        }
    }
}

/// The SHA-256 of `text`, in hex.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A repository's key: the first 16 hex digits of the SHA-256 of its [canonical
/// location](canonical_location).
pub fn repo_key(canonical: &str) -> String {
    sha256_hex(canonical)[..16].to_string()
}

/// The files of one repository's engine in a state directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnginePaths {
    /// `engines/`.
    pub dir: PathBuf,
    pub json: PathBuf,
    pub lock: PathBuf,
    pub token: PathBuf,
    pub log: PathBuf,
}

impl EnginePaths {
    /// The files for the repository at `canonical` under `state_dir`.
    pub fn new(state_dir: &Path, canonical: &str) -> Self {
        let dir = state_dir.join("engines");
        let key = repo_key(canonical);
        EnginePaths {
            json: dir.join(format!("{key}.json")),
            lock: dir.join(format!("{key}.lock")),
            token: dir.join(format!("{key}.token")),
            log: dir.join(format!("{key}.log")),
            dir,
        }
    }

    /// The engine file, when one is there and readable; it may be left over (see
    /// [`EnginePaths::is_locked`]).
    pub fn read(&self) -> Option<EngineFile> {
        serde_json::from_str(&std::fs::read_to_string(&self.json).ok()?).ok()
    }

    /// Whether an engine holds the repository's lock. Checked with a shared lock taken and let go
    /// at once, which an engine's exclusive lock refuses.
    pub fn is_locked(&self) -> bool {
        let Ok(f) = std::fs::File::open(&self.lock) else {
            return false;
        };
        match f.try_lock_shared() {
            Ok(()) => {
                let _ = f.unlock();
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_directory_gets_the_spelling_it_will_have() {
        let dir = std::env::temp_dir().join(format!("vigily-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = dir.join("a").join("..").join("b").join("runs");
        let before = canonical_location(repo.to_str().unwrap());
        std::fs::create_dir_all(dir.join("b").join("runs")).unwrap();
        let after = canonical_location(repo.to_str().unwrap());
        assert_eq!(before, after);
        assert_eq!(repo_key(&before).len(), 16);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uris_keep_their_spelling() {
        assert_eq!(canonical_location("s3://bucket/runs/"), "s3://bucket/runs");
        let p = EnginePaths::new(Path::new("/state"), "s3://bucket/runs");
        assert!(p
            .json
            .ends_with(format!("{}.json", repo_key("s3://bucket/runs"))));
        assert_eq!(p.dir, Path::new("/state/engines"));
    }
}
