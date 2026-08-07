//! Turning declared `shader "<name>"` blocks into renderer-ready injections.
//!
//! Sits between the lowered [`SceneGraph`] — which knows a shader's *name*,
//! *source path* and *declared params* but never opens the file — and
//! [`super::shaders::user_shader`], which assembles GLSL snippets into the
//! viewport's fragment program.
//!
//! Three things happen here:
//!
//! 1. **Only used shaders are resolved.** A declaration no material references
//!    is skipped entirely. That keeps unused-but-broken GLSL from taking the
//!    whole program down with it — a scene renders as long as the shaders it
//!    actually uses compile.
//! 2. **Ids are assigned** from [`FIRST_USER_SHADER_ID`], deterministically by
//!    name so the same scene always produces the same program and the
//!    rebuild-on-change check doesn't thrash.
//! 3. **Params are resolved per material**: declared default, overridden by the
//!    material's `shader_params`. A material naming a param the shader never
//!    declared is ignored (the validator already warned, `W0108`), and an
//!    override whose type doesn't match the declaration falls back to the
//!    default rather than uploading garbage.
//!
//! Source is read at flatten time rather than cached in the renderer, so
//! editing a `.glsl` and recompiling the `.mog` picks the new text up for free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mogen_core::{SceneGraph, ShaderParamValue};

use super::shaders::user_shader::{InjectedShader, ParamDecl, FIRST_USER_SHADER_ID};

/// A declared shader resolved for preview.
#[derive(Debug, Clone)]
pub struct ResolvedShader {
    pub name: String,
    /// Material-shader id, `>= FIRST_USER_SHADER_ID`.
    pub id: i32,
    /// GLSL snippet text, read from `ShaderDecl::source`.
    pub source: String,
    /// Declared params, in declaration order — this is what becomes uniforms.
    pub params: Vec<ParamDecl>,
    /// Declared `default` per param, for params that carry one. Kept here
    /// rather than on [`ParamDecl`] so the renderer's ABI type stays free of
    /// authoring concerns.
    pub defaults: BTreeMap<String, ShaderParamValue>,
}

impl ResolvedShader {
    pub fn injected(&self) -> InjectedShader {
        InjectedShader {
            id: self.id,
            source: self.source.clone(),
            params: self.params.clone(),
        }
    }
}

/// Why a used shader could not be previewed. Surfaced so a silent fall-back to
/// flat PBR isn't the only feedback an author gets.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderLoadError {
    pub name: String,
    pub message: String,
}

/// Resolved shaders plus the name → id map batches use to pick their branch.
#[derive(Debug, Clone, Default)]
pub struct ResolvedShaders {
    pub shaders: Vec<ResolvedShader>,
    pub ids: BTreeMap<String, i32>,
    pub errors: Vec<ShaderLoadError>,
}

impl ResolvedShaders {
    pub fn id_for(&self, name: &str) -> Option<i32> {
        self.ids.get(name).copied()
    }

    fn get(&self, name: &str) -> Option<&ResolvedShader> {
        self.shaders.iter().find(|s| s.name == name)
    }

    /// The injection list for the renderer, in id order.
    pub fn injected(&self) -> Vec<InjectedShader> {
        self.shaders.iter().map(|s| s.injected()).collect()
    }
}

/// Read and number every shader the scene's materials actually reference.
///
/// The built-in `water` name is skipped: it still owns the hard-coded id-1
/// branch in the fragment program, and its `stdlib:` source has no file behind
/// it. See krazyjakee/MoGen#107 — once water is a real GLSL client this special
/// case disappears and it flows through here like any other shader.
pub fn resolve(scene: &SceneGraph, base_dir: Option<&Path>) -> ResolvedShaders {
    let mut used: Vec<&str> = scene
        .materials
        .iter()
        .filter_map(|m| m.shader_name.as_deref())
        .filter(|n| *n != mogen_core::shader::WATER)
        .collect();
    used.sort_unstable();
    used.dedup();

    let mut out = ResolvedShaders::default();
    for name in used {
        let Some(decl) = scene.find_shader_scoped(name, None) else {
            // Unresolvable name — the validator already reported E0106.
            continue;
        };
        let path = resolve_source_path(&decl.source, decl.origin.as_deref(), base_dir);
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                out.errors.push(ShaderLoadError {
                    name: name.to_string(),
                    message: format!("{}: {e}", path.display()),
                });
                continue;
            }
        };
        let id = FIRST_USER_SHADER_ID + out.shaders.len() as i32;
        let mut defaults = BTreeMap::new();
        for p in &decl.params {
            if let Some(d) = &p.default {
                defaults.insert(p.name.clone(), d.clone());
            }
        }
        out.shaders.push(ResolvedShader {
            name: name.to_string(),
            id,
            source,
            params: decl
                .params
                .iter()
                .map(|p| ParamDecl { name: p.name.clone(), ty: p.ty })
                .collect(),
            defaults,
        });
        out.ids.insert(name.to_string(), id);
    }
    out
}

/// Resolve a shader's `source` the way texture paths resolve: relative to the
/// `.mog` that declared it. A shader hoisted from an import resolves against
/// that import's directory rather than the root file's, so a self-contained
/// imported module keeps working when included from elsewhere.
fn resolve_source_path(source: &Path, origin: Option<&Path>, base_dir: Option<&Path>) -> PathBuf {
    if source.is_absolute() {
        return source.to_path_buf();
    }
    match origin.and_then(|o| o.parent()).or(base_dir) {
        Some(d) => d.join(source),
        None => source.to_path_buf(),
    }
}

/// Resolve one material's parameter values against its shader's declarations:
/// declared default first, material override second.
///
/// A param that is neither overridden nor defaulted is omitted — GLSL
/// zero-initialises the uniform, which is what `docs/dsl.md` documents a
/// `param` with no `default` to do.
pub fn resolve_params(
    resolved: &ResolvedShaders,
    shader_name: &str,
    overrides: &BTreeMap<String, ShaderParamValue>,
) -> Vec<(String, ShaderParamValue)> {
    let Some(sh) = resolved.get(shader_name) else {
        return Vec::new();
    };
    sh.params
        .iter()
        .filter_map(|d| {
            let v = overrides
                .get(&d.name)
                .filter(|v| v.matches(d.ty))
                .or_else(|| sh.defaults.get(&d.name))?;
            Some((d.name.clone(), v.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_core::{Material, ShaderDecl, ShaderParamDef, ShaderParamType};
    use std::path::PathBuf;

    /// A scratch directory holding a `.glsl`, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("mogen-shader-test-{}-{tag}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn shader(name: &str, source: &str, params: Vec<ShaderParamDef>) -> ShaderDecl {
        let mut d = ShaderDecl::new(name, source);
        d.params = params;
        d
    }

    fn param(name: &str, ty: ShaderParamType, default: Option<ShaderParamValue>) -> ShaderParamDef {
        ShaderParamDef { name: name.into(), ty, default }
    }

    fn material_using(name: &str, shader: &str) -> Material {
        let mut m = Material::new(name);
        m.shader_name = Some(shader.to_string());
        m
    }

    #[test]
    fn used_shader_is_read_and_numbered_from_the_first_user_id() {
        let dir = Scratch::new("numbered");
        dir.write("ripple.glsl", "vec4 fragment() { return vec4(1.0); }");

        let mut scene = SceneGraph::new();
        scene.add_shader(shader("ripple", "ripple.glsl", vec![]));
        scene.add_material(material_using("pond", "ripple"));

        let r = resolve(&scene, Some(&dir.0));
        assert_eq!(r.shaders.len(), 1);
        assert_eq!(r.id_for("ripple"), Some(FIRST_USER_SHADER_ID));
        assert!(r.shaders[0].source.contains("vec4 fragment()"));
        assert!(r.errors.is_empty());
    }

    #[test]
    fn declared_but_unreferenced_shader_is_not_read() {
        let dir = Scratch::new("unused");
        // Deliberately never written to disk: an unused declaration must not
        // even be opened, so a broken one can't break a scene that ignores it.
        let mut scene = SceneGraph::new();
        scene.add_shader(shader("unused", "missing.glsl", vec![]));

        let r = resolve(&scene, Some(&dir.0));
        assert!(r.shaders.is_empty());
        assert!(r.errors.is_empty(), "unused shader must not report an error");
    }

    #[test]
    fn unreadable_source_reports_an_error_and_assigns_no_id() {
        let dir = Scratch::new("missing");
        let mut scene = SceneGraph::new();
        scene.add_shader(shader("ripple", "nope.glsl", vec![]));
        scene.add_material(material_using("pond", "ripple"));

        let r = resolve(&scene, Some(&dir.0));
        assert!(r.shaders.is_empty());
        assert_eq!(r.id_for("ripple"), None, "no id means the batch falls back to PBR");
        assert_eq!(r.errors.len(), 1);
        assert_eq!(r.errors[0].name, "ripple");
    }

    #[test]
    fn builtin_water_never_takes_a_user_id() {
        let dir = Scratch::new("water");
        let mut scene = SceneGraph::new();
        scene.add_material(material_using("lake", mogen_core::shader::WATER));

        let r = resolve(&scene, Some(&dir.0));
        assert!(r.shaders.is_empty());
        assert_eq!(r.id_for(mogen_core::shader::WATER), None);
        assert!(r.errors.is_empty(), "water has no file to fail on");
    }

    #[test]
    fn two_shaders_get_distinct_consecutive_ids() {
        let dir = Scratch::new("two");
        dir.write("a.glsl", "vec4 fragment() { return vec4(0.0); }");
        dir.write("b.glsl", "vec4 fragment() { return vec4(1.0); }");

        let mut scene = SceneGraph::new();
        scene.add_shader(shader("alpha", "a.glsl", vec![]));
        scene.add_shader(shader("beta", "b.glsl", vec![]));
        scene.add_material(material_using("m1", "alpha"));
        scene.add_material(material_using("m2", "beta"));

        let r = resolve(&scene, Some(&dir.0));
        assert_eq!(r.id_for("alpha"), Some(FIRST_USER_SHADER_ID));
        assert_eq!(r.id_for("beta"), Some(FIRST_USER_SHADER_ID + 1));
    }

    #[test]
    fn params_take_the_material_override_then_the_declared_default() {
        let dir = Scratch::new("params");
        dir.write("r.glsl", "vec4 fragment() { return vec4(1.0); }");

        let mut scene = SceneGraph::new();
        scene.add_shader(shader(
            "ripple",
            "r.glsl",
            vec![
                param("speed", ShaderParamType::Float, Some(ShaderParamValue::Float(2.0))),
                param("frequency", ShaderParamType::Float, Some(ShaderParamValue::Float(8.0))),
                param("nodefault", ShaderParamType::Float, None),
            ],
        ));
        scene.add_material(material_using("pond", "ripple"));

        let r = resolve(&scene, Some(&dir.0));
        let mut overrides = BTreeMap::new();
        overrides.insert("speed".to_string(), ShaderParamValue::Float(3.0));

        let got = resolve_params(&r, "ripple", &overrides);
        // `speed` overridden, `frequency` defaulted, `nodefault` omitted so GLSL
        // zero-initialises it.
        assert_eq!(
            got,
            vec![
                ("speed".to_string(), ShaderParamValue::Float(3.0)),
                ("frequency".to_string(), ShaderParamValue::Float(8.0)),
            ]
        );
    }

    #[test]
    fn override_of_the_wrong_type_falls_back_to_the_default() {
        let dir = Scratch::new("mistyped");
        dir.write("r.glsl", "vec4 fragment() { return vec4(1.0); }");

        let mut scene = SceneGraph::new();
        scene.add_shader(shader(
            "ripple",
            "r.glsl",
            vec![param(
                "speed",
                ShaderParamType::Float,
                Some(ShaderParamValue::Float(2.0)),
            )],
        ));
        scene.add_material(material_using("pond", "ripple"));

        let r = resolve(&scene, Some(&dir.0));
        let mut overrides = BTreeMap::new();
        // A vec3 where a float was declared — uploading this would set the
        // wrong uniform arity, so the default must win.
        overrides.insert(
            "speed".to_string(),
            ShaderParamValue::Vec3([1.0, 2.0, 3.0]),
        );

        let got = resolve_params(&r, "ripple", &overrides);
        assert_eq!(got, vec![("speed".to_string(), ShaderParamValue::Float(2.0))]);
    }
}
