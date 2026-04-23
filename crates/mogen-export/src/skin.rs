use serde_json::{json, Value};

use mogen_core::{NodeId, Skin};

use crate::accessor::push_inverse_bind_matrices;
use crate::{Accessor, BufferView};

pub(crate) fn emit_skin(
    skin: &Skin,
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
) -> Value {
    let ibm_acc = push_inverse_bind_matrices(bin, views, accessors, &skin.inverse_bind_matrices);
    let joints: Vec<u32> = skin.joints.iter().map(|NodeId(i)| *i).collect();
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(skin.name.clone()));
    obj.insert("joints".into(), json!(joints));
    obj.insert("inverseBindMatrices".into(), json!(ibm_acc));
    if let Some(root) = skin.skeleton_root {
        obj.insert("skeleton".into(), json!(root.0));
    }
    Value::Object(obj)
}
