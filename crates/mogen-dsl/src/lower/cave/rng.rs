//! Deterministic RNG for cave lowering. Linear-congruential constants
//! identical to `building/rng.rs` and `lower/branch.rs::rand_pm` so the
//! seeding model is consistent across every procedural generator — the same
//! `seed=` produces the same cave on every build.

/// LCG step. Same constants as `building/rng.rs`.
pub(super) fn step(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    *state
}

/// Uniform [0, 1).
pub(super) fn rand_f01(state: &mut u32) -> f32 {
    let bits = (step(state) >> 8) & 0x00FF_FFFF;
    bits as f32 / (1u32 << 24) as f32
}

/// Uniform in `[lo, hi)`. Returns `lo` if the range is empty.
pub(super) fn rand_in(state: &mut u32, lo: f32, hi: f32) -> f32 {
    if hi <= lo {
        return lo;
    }
    lo + rand_f01(state) * (hi - lo)
}

/// Pick a uniform integer in `[0, n)`. Returns 0 if `n == 0`.
pub(super) fn rand_range(state: &mut u32, n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    step(state) % n
}

/// Sub-seed derived from the user's base seed and a salt. Lets each phase of
/// generation (placement, tunnels, decorations) draw from an independent
/// deterministic stream without one phase's draw count perturbing the next.
pub(super) fn sub_seed(base: u32, salt: u32) -> u32 {
    let mixed = base
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(salt.wrapping_mul(0x7F4A_7C15))
        ^ salt.rotate_left(13);
    mixed.max(1)
}
