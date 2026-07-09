//! Physics attributes attached to geometry.
//!
//! mogen does **not** run a physics simulation — this module carries the data
//! an engine needs to *reconstruct* one. It follows the same split as
//! [`crate::Material`]: a named, reusable [`PhysicsMaterial`] declaration (the
//! `physics "oak" (…)` block) that describes how a substance behaves, plus a
//! per-node [`PhysicsBody`] snapshot that resolves that material against the
//! node's real geometry (computed weight + centre of gravity).
//!
//! The whole thing is authored in human words: a substance's heaviness is a
//! `weight` *per cubic metre* (`700kg/m3`), never the jargon "density"; the
//! bounciness knob is `bounce`, never "restitution". Weight literals accept the
//! same unit suffixes as lengths do (`kg`, `g`, `t`, `lb`, `oz`, `st`).
//!
//! The exporter writes a resolved [`PhysicsBody`] to glTF `node.extras.physics`
//! so a downstream importer (e.g. the companion `godot-mog`) can build a
//! `RigidBody3D` + `PhysicsMaterial` without any hand-authoring.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicsId(pub u32);

/// A named, reusable physics substance — the `physics "oak" (…)` declaration.
///
/// Parallel to [`crate::Material`]: declared once at file/scene scope and
/// referenced from geometry with `phys="oak"`. Holds the *intrinsic* properties
/// of the substance; the per-object weight is derived from these plus the real
/// mesh volume at lowering time (see [`PhysicsBody`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsMaterial {
    pub name: String,
    /// Weight per cubic metre — i.e. density, but spelled the way a human reads
    /// it. Kilograms per m³ (the base unit `700kg/m3` normalises to). Multiplied
    /// by a node's real volume to auto-compute its weight. Default `1000.0`
    /// (roughly water).
    pub weight_per_m3: f32,
    /// Surface friction coefficient. `0` is frictionless ice, `1` is grippy
    /// rubber; values above `1` are legal (some engines use them). Default
    /// `0.5`.
    pub friction: f32,
    /// Bounciness (a.k.a. restitution). `0` is a dead thud, `1` is a superball.
    /// Default `0.0`.
    pub bounce: f32,
    /// Canonical path of the imported `.mog` this was hoisted from; `None` when
    /// authored in the file being lowered. Scopes name lookup exactly like
    /// [`crate::Material::origin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PathBuf>,
}

impl PhysicsMaterial {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            weight_per_m3: 1000.0,
            friction: 0.5,
            bounce: 0.0,
            origin: None,
        }
    }
}

/// A resolved physics body stamped onto a single [`crate::SceneNode`].
///
/// This is the self-contained snapshot the exporter serialises: the substance's
/// properties copied off the referenced [`PhysicsMaterial`], plus the two values
/// mogen computes from the node's real, watertight mesh — `mass` (kg) and the
/// volume `center_of_gravity` (local space). Both are `None` when the node has
/// no mesh to weigh (e.g. `phys=` set on a bare `group`), leaving only the
/// substance data for the engine to apply as it sees fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhysicsBody {
    /// Name of the `physics` material this body was resolved from.
    pub material: String,
    pub weight_per_m3: f32,
    pub friction: f32,
    pub bounce: f32,
    /// Computed mass in kilograms (`weight_per_m3 × world_volume`), or an
    /// explicit per-node `weight=<mass>` override. `None` when the node carries
    /// no mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mass: Option<f32>,
    /// Volume centroid (centre of mass, uniform density) in the node's local
    /// mesh space. `None` when the node carries no mesh or the mesh is
    /// degenerate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center_of_gravity: Option<[f32; 3]>,
}
