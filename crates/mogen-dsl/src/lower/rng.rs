//! Deterministic RNG shared by every procedural generator (`branch`,
//! `building`, `cave`, and any future system).
//!
//! One linear-congruential generator, one seeding convention: a `seed=` attr
//! drives the whole subtree, and `mix_seed` derives independent sub-streams for
//! individual phases so one phase's draw count never perturbs another's. Every
//! generator pulling from this module means the same `seed=` is comparable
//! across systems and the constants live in exactly one place.

/// LCG step (Numerical Recipes constants). Returns the new state.
pub(crate) fn step(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    *state
}

/// Uniform float in `[0, 1)`.
pub(crate) fn rand_f01(state: &mut u32) -> f32 {
    let bits = (step(state) >> 8) & 0x00FF_FFFF;
    bits as f32 / (1u32 << 24) as f32
}

/// Uniform float in `[-1, 1]`. The signed form the tree generator wants.
pub(crate) fn rand_pm(state: &mut u32) -> f32 {
    rand_f01(state) * 2.0 - 1.0
}

/// Uniform float in `[lo, hi)`. Returns `lo` if the range is empty.
pub(crate) fn rand_in(state: &mut u32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        return lo;
    }
    lo + rand_f01(state) * (hi - lo)
}

/// Uniform integer in `[0, n)`. Returns 0 if `n == 0`.
pub(crate) fn rand_range(state: &mut u32, n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    step(state) % n
}

/// Derive an independent sub-seed from a base seed and a salt. Used to give each
/// generation phase (placement, tunnels, decorations, layout retries) its own
/// deterministic stream rooted at the same user `seed=`. Always non-zero so the
/// LCG never gets stuck at 0.
pub(crate) fn mix_seed(base: u32, salt: u32) -> u32 {
    let mixed = base
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(salt.wrapping_mul(0x7F4A_7C15))
        ^ salt.rotate_left(13);
    mixed.max(1)
}

/// Pick an index from a discrete distribution weighted by `weights`. Non-finite
/// or non-positive weights are ignored; returns `0` if every weight is zero.
pub(crate) fn weighted_pick(state: &mut u32, weights: &[f32]) -> usize {
    let total: f32 = weights.iter().copied().filter(|w| w.is_finite() && *w > 0.0).sum();
    if total <= 0.0 {
        return 0;
    }
    let r = rand_f01(state) * total;
    let mut acc = 0.0;
    for (i, w) in weights.iter().enumerate() {
        if !(w.is_finite() && *w > 0.0) {
            continue;
        }
        acc += *w;
        if r <= acc {
            return i;
        }
    }
    weights.len().saturating_sub(1)
}
