//! Parameter identities and the build-scoped primitive tessellation cache.
//!
//! The key is deliberately minted from the values handed to the geometry
//! kernel, not the authored attribute spelling. Thus an omitted default and
//! the same explicit value share a key, while LOD-scaled counts and UV mode are
//! already resolved and cannot accidentally alias.

use std::cell::RefCell;
use std::collections::HashMap;

use mogen_core::{Mesh, UvMode};

use crate::ast::Node;

use super::deform;

thread_local! {
    static TESSELLATIONS: RefCell<HashMap<[u8; 32], Mesh>> = RefCell::new(HashMap::new());
    #[cfg(test)]
    static TESSELLATION_MISSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Keeps tessellation sharing local to one lowering call. Nested calls restore
/// the outer cache rather than contaminating it.
pub(super) struct TessellationCacheGuard {
    previous: HashMap<[u8; 32], Mesh>,
}

impl TessellationCacheGuard {
    pub(super) fn fresh() -> Self {
        let previous = TESSELLATIONS.with(|c| std::mem::take(&mut *c.borrow_mut()));
        Self { previous }
    }
}

impl Drop for TessellationCacheGuard {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        TESSELLATIONS.with(|c| *c.borrow_mut() = previous);
    }
}

/// Return the cached tessellation for `identity`, building it once on a miss.
/// SceneNode currently owns its Mesh, so hits clone the retained buffers; the
/// expensive analytic construction itself is nevertheless performed once.
pub(super) fn intern_tessellation(identity: [u8; 32], build: impl FnOnce() -> Mesh) -> Mesh {
    if let Some(mesh) = TESSELLATIONS.with(|c| c.borrow().get(&identity).cloned()) {
        return mesh;
    }
    let mesh = build();
    TESSELLATIONS.with(|c| {
        c.borrow_mut().insert(identity, mesh.clone());
    });
    #[cfg(test)]
    TESSELLATION_MISSES.with(|n| n.set(n.get() + 1));
    mesh
}

pub(super) fn intern_tessellation_result(
    identity: [u8; 32],
    build: impl FnOnce() -> anyhow::Result<Mesh>,
) -> anyhow::Result<Mesh> {
    if let Some(mesh) = TESSELLATIONS.with(|c| c.borrow().get(&identity).cloned()) {
        return Ok(mesh);
    }
    let mesh = build()?;
    TESSELLATIONS.with(|c| {
        c.borrow_mut().insert(identity, mesh.clone());
    });
    #[cfg(test)]
    TESSELLATION_MISSES.with(|n| n.set(n.get() + 1));
    Ok(mesh)
}

/// A compact, type-framed stream into blake3. Every float follows the same
/// canonical rule as downstream mesh hashing (`-0` is `+0`; all NaNs share a
/// payload), so incidental bit spellings do not split an identity.
pub(super) struct GeometryIdentityBuilder(blake3::Hasher);

impl GeometryIdentityBuilder {
    pub(super) fn primitive(kind: &str, uv_mode: UvMode) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"mogen primitive geometry identity v1\0");
        let mut this = Self(h);
        this.str(kind);
        this.bool(matches!(uv_mode, UvMode::Fit));
        this
    }

    fn continuation(base: [u8; 32]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"mogen final geometry identity v1\0");
        h.update(&base);
        Self(h)
    }

    pub(super) fn u32(&mut self, value: u32) {
        self.0.update(b"u");
        self.0.update(&value.to_le_bytes());
    }

    pub(super) fn usize(&mut self, value: usize) {
        self.u32(value as u32);
    }

    pub(super) fn f32(&mut self, value: f32) {
        self.0.update(b"f");
        let bits = if value == 0.0 {
            0
        } else if value.is_nan() {
            0x7fc0_0000
        } else {
            value.to_bits()
        };
        self.0.update(&bits.to_le_bytes());
    }

    pub(super) fn bool(&mut self, value: bool) {
        self.0.update(&[b'b', value as u8]);
    }

    pub(super) fn str(&mut self, value: &str) {
        self.0.update(b"s");
        self.0.update(&(value.len() as u64).to_le_bytes());
        self.0.update(value.as_bytes());
    }

    pub(super) fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

pub(super) trait GeometryParameter {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder);
}

pub(super) fn write_parameter<T: GeometryParameter + ?Sized>(
    h: &mut GeometryIdentityBuilder,
    value: &T,
) {
    value.write_geometry(h);
}

impl GeometryParameter for f32 {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.f32(*self);
    }
}

impl GeometryParameter for u32 {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.u32(*self);
    }
}

impl GeometryParameter for usize {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.usize(*self);
    }
}

impl GeometryParameter for bool {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.bool(*self);
    }
}

impl GeometryParameter for str {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.str(self);
    }
}

impl<T: GeometryParameter> GeometryParameter for [T] {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        h.0.update(b"[");
        h.0.update(&(self.len() as u64).to_le_bytes());
        for value in self {
            value.write_geometry(h);
        }
    }
}

impl<T: GeometryParameter, const N: usize> GeometryParameter for [T; N] {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        self.as_slice().write_geometry(h);
    }
}

impl<T: GeometryParameter> GeometryParameter for Vec<T> {
    fn write_geometry(&self, h: &mut GeometryIdentityBuilder) {
        self.as_slice().write_geometry(h);
    }
}

/// Extend a base tessellation key with every operation performed after the
/// primitive kernel. The anchor contribution is its resolved displacement,
/// not the source token, and subdivision is the LOD-scaled iteration count.
pub(super) fn final_identity(
    base: [u8; 32],
    node: &Node,
    anchor_shift: [f32; 3],
    subdivisions: u32,
) -> [u8; 32] {
    let mut h = GeometryIdentityBuilder::continuation(base);
    deform::update_geometry_identity(&mut h, node);
    write_parameter(&mut h, &anchor_shift);
    h.u32(subdivisions);
    h.finish()
}

#[cfg(test)]
pub(super) fn reset_tessellation_misses() {
    TESSELLATION_MISSES.with(|n| n.set(0));
}

#[cfg(test)]
pub(super) fn tessellation_misses() -> usize {
    TESSELLATION_MISSES.with(|n| n.get())
}
