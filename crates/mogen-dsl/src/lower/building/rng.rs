//! Building lowering draws from the shared procedural RNG so the same `seed=`
//! is comparable across every generator. `attempt_seed` is the building-local
//! name for the shared `mix_seed` (the layout solver uses it to explore N
//! attempts deterministically).

pub(super) use crate::lower::rng::{
    mix_seed as attempt_seed, rand_f01, rand_range, step, weighted_pick,
};
