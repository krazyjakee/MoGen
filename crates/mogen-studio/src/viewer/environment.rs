//! Environment lighting presets driving the viewport's analytic sky probe and
//! fallback key/fill rig. Each preset packs a complete world-lighting setup —
//! sky-dome triband, sun direction/colour, and the warm-key/cool-fill pair the
//! shader uses when the scene declares no DSL `light` nodes — so swapping
//! presets re-lights an unauthored scene end-to-end with one click.
//!
//! Values feed the same uniforms the renderer used to hardcode (see
//! `viewer/renderer.rs::draw`); switching presets just hands a fresh
//! [`EnvironmentParams`] to [`super::renderer::Renderer::set_environment`].

use glam::Vec3;

/// Built-in environment presets selectable from the viewport overlay.
///
/// Persisted by lowercase label (see [`environment_key`]) so new variants can
/// be added without a settings-file migration. The default matches the
/// historical hardcoded look so existing screenshots stay stable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Neutral daylight studio. Soft blue zenith, warm horizon, muted ground —
    /// matches the look every major DCC ships with by default.
    Studio,
    /// Bright noon sky: deep blue zenith, near-white horizon, vivid sun.
    Daylight,
    /// Warm orange-pink horizon with a low sun. Reads as golden hour.
    Sunset,
    /// Even, diffuse grey dome with no sun disc. Good for inspecting form
    /// without strong shadows.
    Overcast,
    /// Dim cool blue dome lit by a faint moon. Useful for emissive scenes.
    Night,
    /// Dim neutral interior light — no sun, soft horizon-only fill.
    Indoor,
    /// No environmental contribution: black sky dome, no sun, zero IBL. The
    /// analytic key/fill fallback (hardcoded in the FS) still illuminates
    /// geometry when the scene has no DSL `light` nodes, so the model stays
    /// visible against the black background; once DSL lights are authored,
    /// only those plus emissives contribute.
    None,
}

pub const ENVIRONMENTS: [Environment; 7] = [
    Environment::Studio,
    Environment::Daylight,
    Environment::Sunset,
    Environment::Overcast,
    Environment::Night,
    Environment::Indoor,
    Environment::None,
];

pub const DEFAULT_ENVIRONMENT: Environment = Environment::Studio;

/// Resolved lighting values handed to the renderer for one preset. Direction
/// vectors are stored unnormalised so the parameter table reads naturally; the
/// renderer normalises before upload.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentParams {
    /// Direction the analytic key light points along (pre-normalisation). The
    /// shader negates this when computing the lambert dot, so picture this as
    /// the vector "from the light toward the scene".
    pub key_dir: Vec3,
    /// Fill counterpart to [`Self::key_dir`].
    pub fill_dir: Vec3,
    /// Sky zenith colour. Pure linear RGB; the shader maps it through AgX.
    pub sky_top: Vec3,
    /// Horizon band colour.
    pub sky_horizon: Vec3,
    /// Ground bounce colour seen at the bottom hemisphere.
    pub sky_ground: Vec3,
    /// Direction the sun disc points along. Same convention as [`Self::key_dir`].
    pub sun_dir: Vec3,
    /// Sun disc colour multiplier — the FS multiplies this by a tight cosine
    /// pow to produce the bright spot in the sky probe.
    pub sun_color: Vec3,
}

pub fn environment_key(e: Environment) -> &'static str {
    match e {
        Environment::Studio => "studio",
        Environment::Daylight => "daylight",
        Environment::Sunset => "sunset",
        Environment::Overcast => "overcast",
        Environment::Night => "night",
        Environment::Indoor => "indoor",
        Environment::None => "none",
    }
}

pub fn environment_label(e: Environment) -> &'static str {
    match e {
        Environment::Studio => "Studio (neutral)",
        Environment::Daylight => "Daylight (noon)",
        Environment::Sunset => "Sunset (warm)",
        Environment::Overcast => "Overcast (soft)",
        Environment::Night => "Night (moonlit)",
        Environment::Indoor => "Indoor (dim)",
        Environment::None => "None (off)",
    }
}

/// Compact label for the viewport overlay button — the parenthetical mood
/// notes from [`environment_label`] make the menu pop wide enough to look
/// out of place in the toolbar.
pub fn environment_short_label(e: Environment) -> &'static str {
    match e {
        Environment::Studio => "Studio",
        Environment::Daylight => "Daylight",
        Environment::Sunset => "Sunset",
        Environment::Overcast => "Overcast",
        Environment::Night => "Night",
        Environment::Indoor => "Indoor",
        Environment::None => "None",
    }
}

pub fn parse_environment(s: &str) -> Option<Environment> {
    match s.trim().to_ascii_lowercase().as_str() {
        "studio" | "" => Some(Environment::Studio),
        "daylight" | "noon" | "day" => Some(Environment::Daylight),
        "sunset" | "golden" | "dusk" => Some(Environment::Sunset),
        "overcast" | "cloudy" => Some(Environment::Overcast),
        "night" | "moonlit" | "moon" => Some(Environment::Night),
        "indoor" | "interior" => Some(Environment::Indoor),
        "none" | "off" | "black" => Some(Environment::None),
        _ => None,
    }
}

impl Default for Environment {
    fn default() -> Self {
        DEFAULT_ENVIRONMENT
    }
}

impl Environment {
    /// Resolve the preset to its concrete renderer params.
    ///
    /// Numbers were tuned in the AgX-tonemapped output, not in raw radiance —
    /// changing them shifts how the lit scene reads, not the on-disk export.
    pub fn params(self) -> EnvironmentParams {
        match self {
            Environment::Studio => EnvironmentParams {
                key_dir: Vec3::new(-0.4, -1.0, -0.3),
                fill_dir: Vec3::new(0.6, -0.2, 0.5),
                sky_top: Vec3::new(0.33, 0.42, 0.57),
                sky_horizon: Vec3::new(0.51, 0.51, 0.49),
                sky_ground: Vec3::new(0.11, 0.10, 0.09),
                sun_dir: Vec3::new(-0.4, -1.0, -0.3),
                sun_color: Vec3::new(0.66, 0.63, 0.57),
            },
            Environment::Daylight => EnvironmentParams {
                key_dir: Vec3::new(-0.3, -1.0, -0.2),
                fill_dir: Vec3::new(0.5, -0.3, 0.4),
                sky_top: Vec3::new(0.18, 0.36, 0.74),
                sky_horizon: Vec3::new(0.74, 0.84, 0.95),
                sky_ground: Vec3::new(0.16, 0.15, 0.13),
                sun_dir: Vec3::new(-0.3, -1.0, -0.2),
                sun_color: Vec3::new(1.10, 1.02, 0.86),
            },
            Environment::Sunset => EnvironmentParams {
                // Low sun coming in nearly horizontal from the front-left so
                // models are rim-lit on one side.
                key_dir: Vec3::new(-0.8, -0.35, -0.3),
                fill_dir: Vec3::new(0.5, -0.1, 0.6),
                sky_top: Vec3::new(0.18, 0.20, 0.36),
                sky_horizon: Vec3::new(0.94, 0.52, 0.30),
                sky_ground: Vec3::new(0.14, 0.08, 0.07),
                sun_dir: Vec3::new(-0.8, -0.35, -0.3),
                sun_color: Vec3::new(1.20, 0.62, 0.30),
            },
            Environment::Overcast => EnvironmentParams {
                // No directional sun in the dome; key/fill from above so
                // there's still a faint top-down gradient on flat surfaces.
                key_dir: Vec3::new(-0.1, -1.0, -0.1),
                fill_dir: Vec3::new(0.1, -0.5, 0.2),
                sky_top: Vec3::new(0.62, 0.64, 0.66),
                sky_horizon: Vec3::new(0.55, 0.56, 0.57),
                sky_ground: Vec3::new(0.20, 0.20, 0.20),
                sun_dir: Vec3::new(-0.1, -1.0, -0.1),
                // Sun colour set to zero so the FS sun-disc term collapses
                // and the sky reads as a uniform overcast dome.
                sun_color: Vec3::ZERO,
            },
            Environment::Night => EnvironmentParams {
                key_dir: Vec3::new(-0.5, -0.8, -0.3),
                fill_dir: Vec3::new(0.4, -0.2, 0.5),
                sky_top: Vec3::new(0.02, 0.03, 0.08),
                sky_horizon: Vec3::new(0.06, 0.08, 0.16),
                sky_ground: Vec3::new(0.01, 0.01, 0.03),
                sun_dir: Vec3::new(-0.5, -0.8, -0.3),
                // Cool moon — dimmer than the sun and pushed blue.
                sun_color: Vec3::new(0.18, 0.22, 0.32),
            },
            Environment::Indoor => EnvironmentParams {
                key_dir: Vec3::new(-0.3, -1.0, -0.4),
                fill_dir: Vec3::new(0.5, -0.3, 0.3),
                sky_top: Vec3::new(0.22, 0.21, 0.19),
                sky_horizon: Vec3::new(0.30, 0.28, 0.25),
                sky_ground: Vec3::new(0.08, 0.07, 0.06),
                sun_dir: Vec3::new(-0.3, -1.0, -0.4),
                // No sun indoors — dome only.
                sun_color: Vec3::ZERO,
            },
            Environment::None => EnvironmentParams {
                // Directions match the Studio preset so the FS fallback rig
                // (used only when `u_num_lights == 0`) still has a sane
                // `normalize(-u_key_dir)` and the geometry isn't lost on a
                // black background. The fallback's key/fill colours are
                // hardcoded in the shader and intentionally outside this
                // struct, so they keep an unauthored model visible while the
                // sky probe + sun disc collapse to black below.
                key_dir: Vec3::new(-0.4, -1.0, -0.3),
                fill_dir: Vec3::new(0.6, -0.2, 0.5),
                sky_top: Vec3::ZERO,
                sky_horizon: Vec3::ZERO,
                sky_ground: Vec3::ZERO,
                sun_dir: Vec3::new(-0.4, -1.0, -0.3),
                sun_color: Vec3::ZERO,
            },
        }
    }
}
