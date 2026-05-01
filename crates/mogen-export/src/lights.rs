use serde_json::{json, Value};

use mogen_core::{Light, LightKind, SceneGraph};

/// Result of walking a scene's lights into the JSON shape required by
/// `KHR_lights_punctual`.
///
/// `lights` is the array that lands at top-level
/// `extensions.KHR_lights_punctual.lights`. `node_to_index[i]` is the index
/// into that array for `scene.nodes[i]`, or `None` for nodes without a light.
pub(crate) struct LightTable {
    pub lights: Vec<Value>,
    pub node_to_index: Vec<Option<usize>>,
}

impl LightTable {
    pub(crate) fn is_empty(&self) -> bool {
        self.lights.is_empty()
    }
}

pub(crate) fn collect_lights(scene: &SceneGraph) -> LightTable {
    let mut lights = Vec::new();
    let mut node_to_index = vec![None; scene.nodes.len()];
    for (i, n) in scene.nodes.iter().enumerate() {
        if let Some(light) = &n.light {
            node_to_index[i] = Some(lights.len());
            lights.push(emit_light(&n.name, light));
        }
    }
    LightTable {
        lights,
        node_to_index,
    }
}

fn emit_light(name: &str, l: &Light) -> Value {
    let mut obj = serde_json::Map::new();
    if !name.is_empty() {
        obj.insert("name".into(), Value::String(name.to_string()));
    }
    obj.insert(
        "type".into(),
        Value::String(
            match l.kind {
                LightKind::Directional => "directional",
                LightKind::Point => "point",
                LightKind::Spot => "spot",
            }
            .to_string(),
        ),
    );
    if l.color != [1.0, 1.0, 1.0] {
        obj.insert("color".into(), json!(l.color));
    }
    if l.intensity != 1.0 {
        obj.insert("intensity".into(), json!(l.intensity));
    }
    if let Some(r) = l.range {
        if matches!(l.kind, LightKind::Point | LightKind::Spot) {
            obj.insert("range".into(), json!(r));
        }
    }
    if matches!(l.kind, LightKind::Spot) {
        // glTF spec defaults: innerConeAngle=0, outerConeAngle=PI/4.
        // Emit only when non-default to keep JSON tight.
        let mut spot = serde_json::Map::new();
        if l.inner_cone_rad != 0.0 {
            spot.insert("innerConeAngle".into(), json!(l.inner_cone_rad));
        }
        if (l.outer_cone_rad - std::f32::consts::FRAC_PI_4).abs() > 1e-6 {
            spot.insert("outerConeAngle".into(), json!(l.outer_cone_rad));
        }
        if !spot.is_empty() {
            obj.insert("spot".into(), Value::Object(spot));
        }
    }
    Value::Object(obj)
}
