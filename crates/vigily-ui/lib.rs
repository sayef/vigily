//! vigily's web app, built (`npm run build`) and embedded: each file's path and contents.

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// Whether this binary carries the built web app rather than the placeholder page.
pub fn is_built() -> bool {
    ASSETS.iter().any(|(name, _)| name.starts_with("assets/"))
}
