//! User-authored preview shaders.
//!
//! A `shader "<name>" { source = "…"; param … }` declaration names an on-disk
//! GLSL fragment snippet and the parameters materials may feed it. Like
//! [`crate::Material`], shaders are hoisted into the [`crate::SceneGraph`] in a
//! dedicated pass and referenced by name (`material (… shader="<name>")`).
//!
//! mogen itself never compiles or runs the GLSL — it only carries the path +
//! parameter values as opaque metadata (see the exporter's `node.extras.shader`
//! projection). MoGen Studio is the one component that actually compiles a
//! shader for live preview. The built-in `water` preset is just the first
//! client of this system: it goes through the exact same path a user shader
//! does rather than being special-cased.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Name of the built-in water shader preset. Seeded into every graph (unless the
/// user declares their own `shader "water"`) so `shader="water"` keeps working
/// with no declaration, exactly as the old `MaterialShader::Water` enum did.
pub const WATER: &str = "water";

/// Every built-in shader name. Referenced by the validator (so `shader="water"`
/// resolves without a declaration) and by the lowering seed.
pub fn builtin_names() -> &'static [&'static str] {
    &[WATER]
}

/// The declared type of a shader `param`. Determines the GLSL uniform type and
/// which [`ShaderParamValue`] variant a supplied value must match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderParamType {
    Float,
    Vec2,
    Vec3,
    Vec4,
    /// An RGB colour — same storage as `Vec3`, but authored as `[r, g, b]` and
    /// declared as `type=color` for clarity in the DSL.
    Color,
}

impl ShaderParamType {
    /// Parse the DSL `type=` spelling. Returns `None` for an unknown type.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "float" => ShaderParamType::Float,
            "vec2" => ShaderParamType::Vec2,
            "vec3" => ShaderParamType::Vec3,
            "vec4" => ShaderParamType::Vec4,
            "color" => ShaderParamType::Color,
            _ => return None,
        })
    }

    /// The GLSL uniform type this parameter compiles to. `Color` is a `vec3`.
    pub fn glsl_type(self) -> &'static str {
        match self {
            ShaderParamType::Float => "float",
            ShaderParamType::Vec2 => "vec2",
            ShaderParamType::Vec3 | ShaderParamType::Color => "vec3",
            ShaderParamType::Vec4 => "vec4",
        }
    }
}

/// A concrete value fed to a shader parameter. Deliberately small — the subset
/// of DSL literals a GLSL uniform can take. `#[serde(untagged)]` keeps the
/// `extras` JSON clean (`3.5`, `[0, 0.3, 0.5]`) rather than tagged variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShaderParamValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}

impl ShaderParamValue {
    /// Whether this value can populate a uniform of the given declared type.
    /// `Color` accepts a `Vec3`.
    pub fn matches(&self, ty: ShaderParamType) -> bool {
        matches!(
            (self, ty),
            (ShaderParamValue::Float(_), ShaderParamType::Float)
                | (ShaderParamValue::Vec2(_), ShaderParamType::Vec2)
                | (
                    ShaderParamValue::Vec3(_),
                    ShaderParamType::Vec3 | ShaderParamType::Color
                )
                | (ShaderParamValue::Vec4(_), ShaderParamType::Vec4)
        )
    }
}

/// A single declared parameter of a [`ShaderDecl`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShaderParamDef {
    pub name: String,
    pub ty: ShaderParamType,
    /// Value used when a referencing material supplies no override. `None` means
    /// the uniform falls back to GLSL's default (zero) if the material is silent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ShaderParamValue>,
}

/// A hoisted `shader "<name>" { … }` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShaderDecl {
    pub name: String,
    /// Path to the GLSL fragment snippet, relative to the `.mog` that declared
    /// it (resolved the same way as texture paths). Built-in presets carry a
    /// `stdlib:`-keyed path that MoGen Studio resolves from bundled bytes.
    pub source: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ShaderParamDef>,
    /// Canonical path of the imported `.mog` this shader was hoisted from, or
    /// `None` for the file currently being lowered. Mirrors [`crate::Material::origin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PathBuf>,
}

impl ShaderDecl {
    pub fn new(name: impl Into<String>, source: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            params: Vec::new(),
            origin: None,
        }
    }

    /// Resolve the effective value of `param`, preferring a material's override,
    /// then the declared default. `None` when the param is unknown and
    /// undefaulted.
    pub fn resolve_param(
        &self,
        name: &str,
        overrides: &BTreeMap<String, ShaderParamValue>,
    ) -> Option<ShaderParamValue> {
        if let Some(v) = overrides.get(name) {
            return Some(v.clone());
        }
        self.params
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.default.clone())
    }
}
