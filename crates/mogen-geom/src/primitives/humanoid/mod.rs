//! Stdlib-embedded humanoid asset table. Every `.glb` in this directory is
//! pulled into the binary at build time by `build.rs`, keyed by relative path
//! (`humanoid/<file>.glb`). Looked up via `src="stdlib:humanoid/<file>.glb"`
//! on the `mesh` primitive.

include!(concat!(env!("OUT_DIR"), "/humanoid_assets.rs"));

/// Bytes of the embedded asset whose key is `key`, or `None` if no such
/// asset is registered. `key` is the path under `humanoid/` — e.g.
/// `"humanoid/head_male_casual.glb"`.
pub fn stdlib_asset(key: &str) -> Option<&'static [u8]> {
    HUMANOID_ASSETS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, b)| *b)
}

/// All registered stdlib asset keys. Useful for diagnostics ("did you mean…?")
/// and for the validator.
pub fn stdlib_keys() -> impl Iterator<Item = &'static str> {
    HUMANOID_ASSETS.iter().map(|(k, _)| *k)
}
