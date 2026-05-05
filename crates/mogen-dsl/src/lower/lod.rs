use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::Node;

use super::LOD_SCALE;

thread_local! {
    /// Per-origin LOD multiplier collected from imported files'
    /// top-level `lod_scale (value=N)` directives. Keyed by the
    /// canonical path stamped onto every imported `Node.origin`.
    /// Read by `LodOriginScaleGuard` at the entry of every
    /// `lower_into` call so each imported file's geometry honours
    /// its own declared LOD even when the importing file specifies
    /// a different one (or none at all).
    static LOD_BY_ORIGIN: RefCell<HashMap<PathBuf, f32>> =
        RefCell::new(HashMap::new());
}

/// Find a top-level `lod_scale (value=N)` declaration and return its multiplier.
/// Defaults to 1.0 when absent. Values <= 0 fall back to 1.0 so a malformed
/// setting can't silently destroy every mesh.
pub(super) fn extract_lod_scale(ast: &[Node]) -> f32 {
    for n in ast {
        if n.kind == "lod_scale" {
            if let Some(v) = n.attr_number("value") {
                if v > 0.0 {
                    return v;
                }
            }
        }
    }
    1.0
}

/// Walk imported declarations for top-level `lod_scale (value=N)` nodes
/// and register `(origin, value)` pairs into the per-origin LOD map.
/// Imports lift this directive verbatim (with `origin` stamped) so each
/// imported file's geometry can be lowered with its own LOD multiplier.
pub(super) fn collect_origin_lods(imported: &[Node]) {
    LOD_BY_ORIGIN.with(|m| {
        let mut map = m.borrow_mut();
        for n in imported {
            if n.kind != "lod_scale" {
                continue;
            }
            let Some(origin) = n.origin.clone() else {
                continue;
            };
            let Some(v) = n.attr_number("value") else {
                continue;
            };
            if v > 0.0 {
                // First decl per origin wins, matching `extract_lod_scale`.
                map.entry(origin).or_insert(v);
            }
        }
    });
}

pub(super) fn current_lod_scale() -> f32 {
    LOD_SCALE.with(|s| s.get())
}

/// RAII guard that resets the per-origin LOD map at the start of a
/// `lower()` call and restores the previous map on drop. Keeps a nested
/// or sequential lower call from inheriting another build's mappings.
pub(super) struct LodByOriginGuard {
    prev: HashMap<PathBuf, f32>,
}

impl LodByOriginGuard {
    pub(super) fn fresh() -> Self {
        let prev = LOD_BY_ORIGIN.with(|m| std::mem::take(&mut *m.borrow_mut()));
        Self { prev }
    }
}

impl Drop for LodByOriginGuard {
    fn drop(&mut self) {
        let prev = std::mem::take(&mut self.prev);
        LOD_BY_ORIGIN.with(|m| *m.borrow_mut() = prev);
    }
}

/// RAII guard that swaps `LOD_SCALE` to the imported file's setting
/// for the duration of one `lower_into` call. Each imported file is
/// self-contained: when it declared `lod_scale (value=N)` we use N;
/// when it didn't we use 1.0 (its own implicit default), not the
/// importing file's scale. Mirrors the `meta` block isolation already
/// in place — a file's top-level directives don't leak across imports.
///
/// Nodes with no origin (the user's own file, stdlib expansions) leave
/// `LOD_SCALE` alone so the user's top-level `lod_scale` continues to
/// apply.
pub(super) enum LodOriginScaleGuard {
    /// LOD_SCALE was swapped; the previous value is restored on drop.
    Active(f32),
    /// No swap occurred; nothing to restore.
    Inert,
}

impl LodOriginScaleGuard {
    pub(super) fn for_origin(origin: Option<&Path>) -> Self {
        let Some(path) = origin else {
            return Self::Inert;
        };
        let scale = LOD_BY_ORIGIN
            .with(|m| m.borrow().get(path).copied())
            .unwrap_or(1.0);
        let prev = LOD_SCALE.with(|s| s.replace(scale));
        Self::Active(prev)
    }
}

impl Drop for LodOriginScaleGuard {
    fn drop(&mut self) {
        if let Self::Active(prev) = *self {
            LOD_SCALE.with(|s| s.set(prev));
        }
    }
}

/// Scale a default segment/ring count by the active LOD multiplier.
/// Clamped to a sensible floor so circles still close.
pub(super) fn scaled_default(default: u32, min: u32) -> u32 {
    let scale = current_lod_scale();
    let scaled = (default as f32 * scale).round();
    if scaled < min as f32 {
        min
    } else {
        scaled as u32
    }
}

/// Icosphere subdivisions are exponential (4× tris per step), so a multiplier
/// translates to an additive offset of round(log2(scale)). Floor at 0.
pub(super) fn scaled_subdivisions(default: u32) -> u32 {
    let scale = current_lod_scale();
    if scale <= 0.0 {
        return default;
    }
    let offset = scale.log2().round() as i32;
    (default as i32 + offset).max(0) as u32
}
