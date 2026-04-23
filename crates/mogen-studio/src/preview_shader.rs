/// Viewport-only preview modes the user can pick from View → Shader.
///
/// These change how the 3D preview looks but never touch the exported GLB —
/// they live entirely inside the renderer, downstream of the `SceneGraph`
/// that `mogen-export` consumes.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewShader {
    #[default]
    Standard,
    Toon,
    Ps1,
    Crt,
    Matcap,
    Wireframe,
}

pub const PREVIEW_SHADERS: [PreviewShader; 6] = [
    PreviewShader::Standard,
    PreviewShader::Toon,
    PreviewShader::Ps1,
    PreviewShader::Crt,
    PreviewShader::Matcap,
    PreviewShader::Wireframe,
];

pub const DEFAULT_PREVIEW_SHADER: PreviewShader = PreviewShader::Standard;

pub fn preview_shader_key(s: PreviewShader) -> &'static str {
    match s {
        PreviewShader::Standard => "standard",
        PreviewShader::Toon => "toon",
        PreviewShader::Ps1 => "ps1",
        PreviewShader::Crt => "crt",
        PreviewShader::Matcap => "matcap",
        PreviewShader::Wireframe => "wireframe",
    }
}

pub fn preview_shader_label(s: PreviewShader) -> &'static str {
    match s {
        PreviewShader::Standard => "Standard (PBR)",
        PreviewShader::Toon => "Toon (cel-shaded)",
        PreviewShader::Ps1 => "PS1 (retro dither)",
        PreviewShader::Crt => "CRT (scanlines)",
        PreviewShader::Matcap => "Matcap (clay)",
        PreviewShader::Wireframe => "Wireframe",
    }
}

pub fn parse_preview_shader(s: &str) -> Option<PreviewShader> {
    match s.trim().to_ascii_lowercase().as_str() {
        "standard" | "pbr" | "" => Some(PreviewShader::Standard),
        "toon" | "cel" => Some(PreviewShader::Toon),
        "ps1" | "retro" => Some(PreviewShader::Ps1),
        "crt" | "scanlines" => Some(PreviewShader::Crt),
        "matcap" | "clay" => Some(PreviewShader::Matcap),
        "wireframe" | "wire" => Some(PreviewShader::Wireframe),
        _ => None,
    }
}

impl PreviewShader {
    /// Integer handed to the GL shader via `u_shader_mode`. Keep in sync with
    /// the `#define`s / branches in `viewer/shaders.rs`.
    pub fn shader_mode(self) -> i32 {
        match self {
            PreviewShader::Standard => 0,
            PreviewShader::Toon => 1,
            PreviewShader::Ps1 => 2,
            PreviewShader::Crt => 3,
            PreviewShader::Matcap => 4,
            // Wireframe uses the standard fragment path; the renderer swaps
            // polygon mode to LINE instead.
            PreviewShader::Wireframe => 0,
        }
    }

    pub fn wants_wireframe(self) -> bool {
        matches!(self, PreviewShader::Wireframe)
    }
}
