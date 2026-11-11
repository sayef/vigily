//! The error type of vigily-core.

use std::fmt;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A run, series or repository does not exist.
    NotFound(String),
    /// An argument was invalid: a malformed query, a context that is not an object.
    InvalidArgument(String),
    /// The storage layer failed.
    Storage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(m) => write!(f, "not found: {m}"),
            Error::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            Error::Storage(m) => write!(f, "storage error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(feature = "kv0")]
impl From<kv0::Error> for Error {
    fn from(e: kv0::Error) -> Self {
        match e {
            kv0::Error::NotFound(m) => Error::NotFound(m),
            kv0::Error::InvalidArgument(m) => Error::InvalidArgument(m),
            other => Error::Storage(other.to_string()),
        }
    }
}
