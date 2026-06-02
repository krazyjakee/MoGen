//! Shared attribute-read helpers for the procedural generators' config
//! readers (`branch`, `building`, `cave`).
//!
//! Every generator turns a node's attrs into a typed config struct, applying a
//! default and clamping defensively (the validator has already rejected the
//! egregious cases, but a value that slipped past shouldn't panic at lowering
//! time). These helpers factor out the four recurring shapes — `seed=`, 0/1
//! boolean flags, non-negative counts, and floored / clamped scalars — so the
//! semantics (especially the flag threshold) stay identical across generators
//! instead of each reader hand-rolling its own.

use crate::ast::Node;

/// Standard `seed=` read: floor at 1 so the deterministic RNG never starts from
/// 0, default 1 when the attr is absent.
pub(super) fn seed(node: &Node) -> u32 {
    node.attr_number("seed")
        .map(|n| (n as i64).max(1) as u32)
        .unwrap_or(1)
}

/// Read a 0/1-style boolean attr. Any value whose magnitude exceeds 0.5 reads
/// as true (so `flag=1` / `flag=0` both work); unset falls back to `default`.
pub(super) fn flag(node: &Node, key: &str, default: bool) -> bool {
    node.attr_number(key)
        .map(|n| n.abs() > 0.5)
        .unwrap_or(default)
}

/// Read a count attr, flooring at `min` and truncating to `u32`. `default` is
/// used when the attr is absent.
pub(super) fn count(node: &Node, key: &str, default: f32, min: f32) -> u32 {
    node.attr_number(key).unwrap_or(default).max(min) as u32
}

/// Read a scalar attr with a lower bound. `default` is used when the attr is
/// absent; the result is floored at `min`.
pub(super) fn scalar(node: &Node, key: &str, default: f32, min: f32) -> f32 {
    node.attr_number(key).unwrap_or(default).max(min)
}

/// Read a scalar attr clamped to `[min, max]`. `default` is used when the attr
/// is absent.
pub(super) fn scalar_clamped(node: &Node, key: &str, default: f32, min: f32, max: f32) -> f32 {
    node.attr_number(key).unwrap_or(default).clamp(min, max)
}

/// Read an integer attr clamped to `[min, max]`. `default` is used when the
/// attr is absent (and is returned as-is, unclamped, matching the existing
/// resolution reader).
pub(super) fn int_clamped(node: &Node, key: &str, default: u32, min: u32, max: u32) -> u32 {
    node.attr_number(key)
        .map(|n| (n as u32).clamp(min, max))
        .unwrap_or(default)
}
