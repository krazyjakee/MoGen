//! Cave lowering draws from the shared procedural RNG so the same `seed=`
//! is comparable across every generator. `sub_seed` is the cave-local name for
//! the shared `mix_seed` — each generation phase (placement, tunnels,
//! decorations) draws from an independent deterministic stream.

pub(super) use crate::lower::rng::{mix_seed as sub_seed, rand_f01, rand_in, rand_range};
