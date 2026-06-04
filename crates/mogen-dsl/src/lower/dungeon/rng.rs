//! Dungeon lowering draws from the shared procedural RNG so the same `seed=`
//! is comparable across every generator. `sub_seed` is the dungeon-local name
//! for the shared `mix_seed` — each generation phase (room placement, corridors,
//! stairs, props) draws from an independent deterministic stream.

pub(super) use crate::lower::rng::{mix_seed as sub_seed, rand_range};
