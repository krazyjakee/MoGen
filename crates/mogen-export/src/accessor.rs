use mogen_core::{Mesh, Track, TrackProperty};

use crate::{align_up, bounds, Accessor, BufferView};

/// glTF buffer-view `target` constants ([spec][1]).
///
/// [1]: https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#reference-bufferview
const TARGET_ARRAY_BUFFER: u32 = 34962;
const TARGET_ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// glTF accessor `componentType` constants.
const COMPONENT_F32: u32 = 5126;
const COMPONENT_U16: u32 = 5123;
const COMPONENT_U32: u32 = 5125;

/// One buffer-view + accessor record paired with the byte range it covers
/// in `bin`. `min_max` is `Some` only for accessors required by the glTF spec
/// to declare bounds (POSITION and animation-input keyframes).
struct ViewAccessor {
    byte_offset: usize,
    byte_length: usize,
    target: Option<u32>,
    component_type: u32,
    count: usize,
    ty: &'static str,
    min_max: Option<(Vec<f32>, Vec<f32>)>,
}

fn push_view_and_accessor(
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    desc: ViewAccessor,
) -> usize {
    views.push(BufferView {
        buffer: 0,
        byte_offset: desc.byte_offset,
        byte_length: desc.byte_length,
        target: desc.target,
    });
    let (min, max) = match desc.min_max {
        Some((mn, mx)) => (Some(mn), Some(mx)),
        None => (None, None),
    };
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: desc.component_type,
        count: desc.count,
        ty: desc.ty,
        min,
        max,
    });
    accessors.len() - 1
}

pub(crate) fn push_positions(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for v in &mesh.positions {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let (min, max) = bounds(&mesh.positions);
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.positions.len() * 12,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_F32,
        count: mesh.positions.len(),
        ty: "VEC3",
        min_max: Some((min.to_vec(), max.to_vec())),
    })
}

pub(crate) fn push_normals(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for v in &mesh.normals {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.normals.len() * 12,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_F32,
        count: mesh.normals.len(),
        ty: "VEC3",
        min_max: None,
    })
}

pub(crate) fn push_uvs(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
    scale: [f32; 2],
) -> usize {
    let offset = align_up(bin, 4);
    for uv in &mesh.uvs {
        bin.extend_from_slice(&(uv[0] * scale[0]).to_le_bytes());
        bin.extend_from_slice(&(uv[1] * scale[1]).to_le_bytes());
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.uvs.len() * 8,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_F32,
        count: mesh.uvs.len(),
        ty: "VEC2",
        min_max: None,
    })
}

pub(crate) fn push_colors(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for c in &mesh.colors {
        for ch in c {
            bin.extend_from_slice(&ch.to_le_bytes());
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.colors.len() * 16,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_F32,
        count: mesh.colors.len(),
        ty: "VEC4",
        min_max: None,
    })
}

pub(crate) fn push_indices(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    // align_up(.., 4) keeps the next view aligned for either component size.
    let offset = align_up(bin, 4);
    let use_u16 = mesh.positions.len() <= u16::MAX as usize;
    let (byte_length, component_type) = if use_u16 {
        for i in &mesh.indices {
            bin.extend_from_slice(&(*i as u16).to_le_bytes());
        }
        (mesh.indices.len() * 2, COMPONENT_U16)
    } else {
        for i in &mesh.indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        (mesh.indices.len() * 4, COMPONENT_U32)
    };
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length,
        target: Some(TARGET_ELEMENT_ARRAY_BUFFER),
        component_type,
        count: mesh.indices.len(),
        ty: "SCALAR",
        min_max: None,
    })
}

pub(crate) fn push_times(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    times: &[f32],
) -> usize {
    let offset = align_up(bin, 4);
    for t in times {
        bin.extend_from_slice(&t.to_le_bytes());
    }
    // Animation input accessors must declare min/max per the glTF spec.
    let (mut min_t, mut max_t) = (f32::INFINITY, f32::NEG_INFINITY);
    for t in times {
        if *t < min_t { min_t = *t; }
        if *t > max_t { max_t = *t; }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: times.len() * 4,
        target: None,
        component_type: COMPONENT_F32,
        count: times.len(),
        ty: "SCALAR",
        min_max: Some((vec![min_t], vec![max_t])),
    })
}

pub(crate) fn push_inverse_bind_matrices(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    ibms: &[[[f32; 4]; 4]],
) -> usize {
    let offset = align_up(bin, 4);
    // glTF stores MAT4 column-major. `Mat4::to_cols_array_2d` already returns
    // columns-first ([[col0], [col1], [col2], [col3]]), so a straight float
    // dump in row-major-of-columns order hits the spec.
    for m in ibms {
        for col in m {
            for c in col {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: ibms.len() * 64,
        target: None,
        component_type: COMPONENT_F32,
        count: ibms.len(),
        ty: "MAT4",
        min_max: None,
    })
}

pub(crate) fn push_joints(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for row in &mesh.joints {
        for j in row {
            bin.extend_from_slice(&j.to_le_bytes());
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.joints.len() * 8,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_U16,
        count: mesh.joints.len(),
        ty: "VEC4",
        min_max: None,
    })
}

pub(crate) fn push_weights(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for row in &mesh.weights {
        for w in row {
            bin.extend_from_slice(&w.to_le_bytes());
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: mesh.weights.len() * 16,
        target: Some(TARGET_ARRAY_BUFFER),
        component_type: COMPONENT_F32,
        count: mesh.weights.len(),
        ty: "VEC4",
        min_max: None,
    })
}

pub(crate) fn push_track_values(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    track: &Track,
) -> usize {
    let offset = align_up(bin, 4);
    let (ty, components): (&'static str, usize) = match track.property {
        TrackProperty::Rotation => ("VEC4", 4),
        TrackProperty::Translation | TrackProperty::Scale => ("VEC3", 3),
    };
    for v in &track.values {
        for c in &v[..components] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    push_view_and_accessor(views, accessors, ViewAccessor {
        byte_offset: offset,
        byte_length: track.values.len() * components * 4,
        target: None,
        component_type: COMPONENT_F32,
        count: track.values.len(),
        ty,
        min_max: None,
    })
}
