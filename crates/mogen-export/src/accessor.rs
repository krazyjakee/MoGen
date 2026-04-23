use mogen_core::{Mesh, Track, TrackProperty};

use crate::{align_up, bounds, Accessor, BufferView};

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
    let byte_length = mesh.positions.len() * 12;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    let (min, max) = bounds(&mesh.positions);
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.positions.len(),
        ty: "VEC3",
        min: Some(min.to_vec()),
        max: Some(max.to_vec()),
    });
    accessors.len() - 1
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
    let byte_length = mesh.normals.len() * 12;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.normals.len(),
        ty: "VEC3",
        min: None,
        max: None,
    });
    accessors.len() - 1
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
    let byte_length = mesh.uvs.len() * 8;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.uvs.len(),
        ty: "VEC2",
        min: None,
        max: None,
    });
    accessors.len() - 1
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
        (mesh.indices.len() * 2, 5123u32) // UNSIGNED_SHORT
    } else {
        for i in &mesh.indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        (mesh.indices.len() * 4, 5125u32) // UNSIGNED_INT
    };
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34963) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type,
        count: mesh.indices.len(),
        ty: "SCALAR",
        min: None,
        max: None,
    });
    accessors.len() - 1
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
    let byte_length = times.len() * 4;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    // Animation input accessors must declare min/max per the glTF spec.
    let (mut min_t, mut max_t) = (f32::INFINITY, f32::NEG_INFINITY);
    for t in times {
        if *t < min_t { min_t = *t; }
        if *t > max_t { max_t = *t; }
    }
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: times.len(),
        ty: "SCALAR",
        min: Some(vec![min_t]),
        max: Some(vec![max_t]),
    });
    accessors.len() - 1
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
    let byte_length = ibms.len() * 64;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: ibms.len(),
        ty: "MAT4",
        min: None,
        max: None,
    });
    accessors.len() - 1
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
    let byte_length = mesh.joints.len() * 8;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5123, // UNSIGNED_SHORT
        count: mesh.joints.len(),
        ty: "VEC4",
        min: None,
        max: None,
    });
    accessors.len() - 1
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
    let byte_length = mesh.weights.len() * 16;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126, // FLOAT
        count: mesh.weights.len(),
        ty: "VEC4",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

pub(crate) fn push_track_values(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    track: &Track,
) -> usize {
    let offset = align_up(bin, 4);
    let ty = match track.property {
        TrackProperty::Rotation => "VEC4",
        TrackProperty::Translation | TrackProperty::Scale => "VEC3",
    };
    let components = if ty == "VEC4" { 4 } else { 3 };
    for v in &track.values {
        for c in &v[..components] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let byte_length = track.values.len() * components * 4;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: track.values.len(),
        ty: if ty == "VEC4" { "VEC4" } else { "VEC3" },
        min: None,
        max: None,
    });
    accessors.len() - 1
}
