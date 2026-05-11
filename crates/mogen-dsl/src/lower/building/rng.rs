//! Deterministic RNG for building lowering. Linear-congruential constants
//! identical to `lower/branch.rs::rand_pm` so the seeding model is consistent
//! across procedural generators.

/// LCG step. Same constants as `branch.rs` so deterministic seeds round-trip
/// across both generators.
pub(super) fn step(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    *state
}

/// Uniform [0, 1).
pub(super) fn rand_f01(state: &mut u32) -> f32 {
    let bits = (step(state) >> 8) & 0x00FF_FFFF;
    bits as f32 / (1u32 << 24) as f32
}

/// Pick a uniform integer in [0, n). Panics if `n == 0`.
pub(super) fn rand_range(state: &mut u32, n: u32) -> u32 {
    step(state) % n
}

/// Sub-seed derived from the user's base seed and an attempt index. Used by
/// the layout solver to explore N attempts deterministically.
pub(super) fn attempt_seed(base: u32, attempt: u32) -> u32 {
    let mixed = base
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(attempt.wrapping_mul(0x7F4A_7C15))
        ^ attempt.rotate_left(13);
    mixed.max(1)
}

/// Pick an index from a discrete distribution weighted by `weights`. Returns
/// `0` if all weights are zero.
pub(super) fn weighted_pick(state: &mut u32, weights: &[f32]) -> usize {
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
