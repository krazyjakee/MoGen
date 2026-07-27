//! Assembly of the mesh fragment program from user-authored GLSL snippets.
//!
//! The viewport runs a single fragment program. To preview user shaders without
//! rewriting the per-material uniform/draw path, we keep that one program and
//! *inject* each user shader as its own `fragment_N()` function, wired up by a
//! generated `mogen_user_dispatch(int id)`. The base program in
//! [`super::mesh`] carries a text marker ([`PLACEHOLDER`]) just before `main`;
//! [`assemble_fs`] swaps it for either a no-op stub (standard program) or the
//! injected functions.
//!
//! ## The fragment ABI
//!
//! A user `.glsl` snippet is a *body*, not a whole program. It runs against the
//! prelude the base shader already defines — the varyings (`v_world_pos`,
//! `v_normal`, `v_uv`, `v_color`), the material/frame uniforms (`u_time`,
//! `u_camera_pos`, `u_base_color`, `u_roughness`, …), and the helper functions
//! (`sample_sky`, `fresnel`, the water turbulence, …). It must define
//! `vec4 fragment()` returning the final RGBA ("replace" contract — the shader
//! does its own lighting; standard PBR is bypassed).
//!
//! Each shader's declared `param`s become uniforms, namespaced per shader id so
//! two shaders can both declare a `speed` without colliding, and `#define`d back
//! to their bare names inside the snippet so the author just writes `speed`.

use mogen_core::ShaderParamType;

/// Marker in the base fragment source that [`assemble_fs`] replaces. Must match
/// the literal line in [`super::mesh`]'s `FS_SRC` exactly.
pub const PLACEHOLDER: &str = "//@MOGEN_USER_SHADER_DISPATCH@";

/// The first material-shader id handed to injected user shaders. `0` is standard
/// PBR and `1` is the built-in water branch (still hard-coded in `main` for
/// now), so user shaders start at `2`. Consumed by the flatten→renderer
/// integration that assigns ids to the scene's declared shaders.
// TODO(shader-preview): wired once flatten bakes the used-shader list.
#[allow(dead_code)]
pub const FIRST_USER_SHADER_ID: i32 = 2;

/// One shader to inject into the fragment program.
#[derive(Debug, Clone)]
pub struct InjectedShader {
    /// Material-shader id this snippet answers to (matches `u_material_shader`).
    /// Must be `>= FIRST_USER_SHADER_ID`.
    pub id: i32,
    /// The user GLSL snippet — defines `vec4 fragment()`.
    pub source: String,
    /// Declared parameters, surfaced as namespaced uniforms.
    pub params: Vec<ParamDecl>,
}

/// A single declared parameter (name + GLSL type). Mirrors the relevant fields
/// of [`mogen_core::ShaderParamDef`] without pulling defaults into the renderer.
#[derive(Debug, Clone)]
pub struct ParamDecl {
    pub name: String,
    pub ty: ShaderParamType,
}

/// The GLSL uniform name a shader parameter compiles to. Namespaced by shader id
/// so distinct shaders never collide at file scope. The draw path uploads each
/// material's resolved value to this uniform.
pub fn param_uniform_name(id: i32, param: &str) -> String {
    format!("u_sh{id}_{param}")
}

/// Build the complete fragment shader source. `shaders` empty yields the
/// standard program (marker → dispatch stub), byte-identical in behaviour to
/// the pre-feature shader. Otherwise the marker becomes the injected
/// `fragment_N()` functions plus the dispatch that `main` calls for ids >= 2.
pub fn assemble_fs(shaders: &[InjectedShader]) -> String {
    let block = if shaders.is_empty() {
        // Stub: keeps `main`'s `mogen_user_dispatch` call well-defined when the
        // scene declares no shaders. Never reached (main only calls it for
        // id >= 2, which can't occur without an injected shader).
        "vec4 mogen_user_dispatch(int id) { return vec4(0.0); }\n".to_string()
    } else {
        let mut s = String::new();
        for sh in shaders {
            s.push_str(&format!("// ---- injected shader id {} ----\n", sh.id));
            // Namespaced param uniforms, then `#define`s so the snippet can use
            // the bare param name.
            for p in &sh.params {
                s.push_str(&format!(
                    "uniform {} {};\n",
                    p.ty.glsl_type(),
                    param_uniform_name(sh.id, &p.name)
                ));
            }
            for p in &sh.params {
                s.push_str(&format!(
                    "#define {} {}\n",
                    p.name,
                    param_uniform_name(sh.id, &p.name)
                ));
            }
            // Rename the snippet's `fragment` entrypoint to a unique symbol via
            // the preprocessor (whole-token match — leaves other identifiers
            // untouched), so multiple snippets coexist.
            s.push_str(&format!("#define fragment fragment_{}\n", sh.id));
            s.push_str(&sh.source);
            s.push_str("\n#undef fragment\n");
            for p in &sh.params {
                s.push_str(&format!("#undef {}\n", p.name));
            }
        }
        s.push_str("vec4 mogen_user_dispatch(int id) {\n");
        for sh in shaders {
            s.push_str(&format!(
                "    if (id == {}) return fragment_{}();\n",
                sh.id, sh.id
            ));
        }
        s.push_str("    return vec4(0.0);\n}\n");
        s
    };

    debug_assert!(
        super::FS_SRC.contains(PLACEHOLDER),
        "base fragment source is missing the user-shader placeholder"
    );
    super::FS_SRC.replace(PLACEHOLDER, &block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_present_in_base_source() {
        assert!(
            super::super::FS_SRC.contains(PLACEHOLDER),
            "mesh FS_SRC must carry the injection marker"
        );
        // `main` must call the dispatch so injected shaders are reachable.
        assert!(super::super::FS_SRC.contains("mogen_user_dispatch(u_material_shader)"));
    }

    #[test]
    fn standard_program_defines_stub_and_no_user_functions() {
        let fs = assemble_fs(&[]);
        assert!(!fs.contains(PLACEHOLDER), "marker must be replaced");
        assert!(fs.contains("vec4 mogen_user_dispatch(int id) { return vec4(0.0); }"));
        // No injected entrypoints or dispatch branches in the standard program.
        assert!(!fs.contains("#define fragment fragment_"));
        assert!(!fs.contains("return fragment_"));
        // The standard main body is preserved.
        assert!(fs.contains("void main()"));
        assert!(fs.contains("out vec4 frag;"));
    }

    #[test]
    fn injected_shader_wires_dispatch_params_and_entrypoint() {
        let shaders = vec![InjectedShader {
            id: 2,
            source: "vec4 fragment() { return vec4(tint * speed, 1.0); }".to_string(),
            params: vec![
                ParamDecl { name: "speed".into(), ty: ShaderParamType::Float },
                ParamDecl { name: "tint".into(), ty: ShaderParamType::Color },
            ],
        }];
        let fs = assemble_fs(&shaders);
        // Namespaced uniforms with correct GLSL types (color -> vec3).
        assert!(fs.contains("uniform float u_sh2_speed;"));
        assert!(fs.contains("uniform vec3 u_sh2_tint;"));
        // Bare-name defines so the snippet compiles unchanged.
        assert!(fs.contains("#define speed u_sh2_speed"));
        assert!(fs.contains("#define tint u_sh2_tint"));
        // Entrypoint rename + dispatch wiring.
        assert!(fs.contains("#define fragment fragment_2"));
        assert!(fs.contains("if (id == 2) return fragment_2();"));
        assert!(fs.contains("#undef fragment"));
        assert!(!fs.contains(PLACEHOLDER));
    }

    #[test]
    fn multiple_shaders_get_distinct_namespaces() {
        let shaders = vec![
            InjectedShader {
                id: 2,
                source: "vec4 fragment() { return vec4(speed); }".into(),
                params: vec![ParamDecl { name: "speed".into(), ty: ShaderParamType::Float }],
            },
            InjectedShader {
                id: 3,
                source: "vec4 fragment() { return vec4(speed); }".into(),
                params: vec![ParamDecl { name: "speed".into(), ty: ShaderParamType::Float }],
            },
        ];
        let fs = assemble_fs(&shaders);
        assert!(fs.contains("uniform float u_sh2_speed;"));
        assert!(fs.contains("uniform float u_sh3_speed;"));
        assert!(fs.contains("if (id == 2) return fragment_2();"));
        assert!(fs.contains("if (id == 3) return fragment_3();"));
    }
}
