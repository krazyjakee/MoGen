use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::Node;

use super::LOD_SCALE;

thread_local! {
    // Subtree multipliers are independent of file defaults: switching origin
    // must not erase a containing group's `lod=` (even within the same file).
    static LOD_MULTIPLIER: Cell<f32> = const { Cell::new(1.0) };

    /// Per-origin LOD multiplier collected from imported files'
    /// top-level `lod_scale (value=N)` directives. Keyed by the
    /// canonical path stamped onto every imported `Node.origin`.
    /// Read by `LodOriginScaleGuard` at the entry of every
    /// `lower_into` call so each imported file's geometry honours
    /// its own declared LOD even when the importing file specifies
    /// a different one (or none at all).
    static LOD_BY_ORIGIN: RefCell<HashMap<PathBuf, f32>> =
        RefCell::new(HashMap::new());

    /// The **caller's** tessellation density for this lowering, set by
    /// [`crate::lower::lower_with_loader_lod`]. 1.0 = exactly what the source
    /// asked for, which is what every other entry point passes.
    ///
    /// Deliberately a *second* thread-local rather than a different starting
    /// value for `LOD_SCALE`, and that is the load-bearing part. `LOD_SCALE` is
    /// **replaced** — not multiplied — by [`LodOriginScaleGuard`] on entry to
    /// every imported subtree, precisely so one file's `lod_scale` cannot leak
    /// across an `import`. A caller's request folded into `LOD_SCALE` would be
    /// erased by that replacement, so asking for a coarse bake would coarsen the
    /// root file and silently leave every imported subtree at full density.
    ///
    /// Kept separate it multiplies through *all* of it — the file's own
    /// `lod_scale`, each import's, and every per-node `lod=` — which is what
    /// "give me this whole scene at a quarter density" has to mean.
    static LOD_REQUEST: Cell<f32> = const { Cell::new(1.0) };
}

/// RAII guard publishing the caller's requested density for one `lower()` call.
///
/// Non-positive and non-finite requests fall back to 1.0 — the rule
/// [`extract_lod_scale`] already applies to the DSL directive, for the same
/// reason: a malformed setting must not silently destroy every mesh.
pub(super) struct LodRequestGuard {
    prev: f32,
}

impl LodRequestGuard {
    pub(super) fn set(scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        Self { prev: LOD_REQUEST.with(|s| s.replace(scale)) }
    }
}

impl Drop for LodRequestGuard {
    fn drop(&mut self) {
        let prev = self.prev;
        LOD_REQUEST.with(|s| s.set(prev));
    }
}

/// Find a top-level `lod_scale (value=N)` declaration and return its multiplier.
/// Defaults to 1.0 when absent. Non-finite values and values <= 0 are ignored so a malformed
/// setting can't silently destroy every mesh.
pub(super) fn extract_lod_scale(ast: &[Node]) -> f32 {
    for n in ast {
        if n.kind == "lod_scale" {
            if let Some(v) = n.attr_number("value") {
                if v.is_finite() && v > 0.0 {
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
            if v.is_finite() && v > 0.0 {
                // First decl per origin wins, matching `extract_lod_scale`.
                map.entry(origin).or_insert(v);
            }
        }
    });
}

/// The density every primitive tessellates at right now: the source's own
/// file scale, multiplied by enclosing per-node overrides and the caller's request.
///
/// A product rather than a choice between the two: an author who marked a hero
/// prop `lod=2` means "twice whatever else is going on", and that stays true at
/// every density a baker asks for. Overriding instead would flatten a scene's
/// authored detail hierarchy the moment anything requested a LOD.
pub(super) fn current_lod_scale() -> f32 {
    LOD_SCALE.with(|s| s.get()) * LOD_MULTIPLIER.with(|s| s.get()) * LOD_REQUEST.with(|s| s.get())
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

/// RAII guard that accumulates a per-node `lod=N` attribute for
/// the duration of one `lower_into` call. Lets authors mark hero parts
/// (`lod=2.0`) and background parts (`lod=0.5`) without touching the
/// file-global `lod_scale (value=N)`.
///
/// The multiplier compounds with the active scale — a `lod=2.0` on a
/// subtree inside a file with a top-level `lod_scale (value=0.5)` ends up
/// at an effective scale of `1.0`, matching what an author would expect
/// from a "double the detail of this part" override. Non-finite and non-positive values
/// are ignored (treated as no-op) so a malformed override can't silently
/// destroy every mesh in the subtree.
pub(super) enum LodMultiplierGuard {
    /// Active multiplier applied; the previous subtree multiplier is restored on drop.
    Active(f32),
    /// Either no `lod` attr present or its value was non-positive — no swap.
    Inert,
}

impl LodMultiplierGuard {
    pub(super) fn fresh() -> Self {
        Self::Active(LOD_MULTIPLIER.with(|s| s.replace(1.0)))
    }

    pub(super) fn for_node(node: &Node) -> Self {
        let Some(mult) = node.attr_number("lod") else {
            return Self::Inert;
        };
        if !mult.is_finite() || mult <= 0.0 {
            return Self::Inert;
        }
        let prev = LOD_MULTIPLIER.with(|s| {
            let cur = s.get();
            s.replace(cur * mult)
        });
        Self::Active(prev)
    }
}

impl Drop for LodMultiplierGuard {
    fn drop(&mut self) {
        if let Self::Active(prev) = *self {
            LOD_MULTIPLIER.with(|s| s.set(prev));
        }
    }
}

/// Scale a segment/ring/sample count by the active LOD multiplier.
/// Used for both primitive defaults and author-supplied explicit values
/// (e.g. `segments_u=64`) so a global `lod_scale` or per-node `lod=`
/// keeps working on dense surfaces — heightfields, curved planes, lathes —
/// where authors typically pin the segment count to control noise quality.
/// Clamped to a sensible floor so circles still close.
pub(super) fn scaled_count(value: u32, min: u32) -> u32 {
    let scale = current_lod_scale();
    let scaled = (value as f32 * scale).round();
    if scaled < min as f32 {
        min
    } else {
        scaled as u32
    }
}

/// Icosphere subdivisions are exponential (4× tris per step), so a multiplier
/// translates to an additive offset of round(log2(scale)). Floor at 0.
/// Applied to both the implicit default and an author-supplied `subdivisions=`.
pub(super) fn scaled_subdivisions(value: u32) -> u32 {
    let scale = current_lod_scale();
    if scale <= 0.0 {
        return value;
    }
    let offset = scale.log2().round() as i32;
    (value as i32 + offset).max(0) as u32
}
