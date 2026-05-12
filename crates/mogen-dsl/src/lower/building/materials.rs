//! Auto-created default materials for building openings.
//!
//! The stdlib `door_simple`, `window_simple`, and `skylight_simple` modules
//! reference named materials (`door_frame`, `window_frame`, `window_glass`,
//! `skylight_frame`) so each opening reads visually distinct from the wall it
//! cuts through. The wrapper material the user attaches to the `building`
//! node is meant for plaster / stone — not panes and trim — so we synthesise
//! those defaults here if the user hasn't declared them.
//!
//! Per-opening-kind slab materials (`ext_door` for entrances, `int_door` for
//! interior doors) are bound by `emit::modules` onto the wrapping opening
//! group; the stdlib `door_simple` slab inherits, so a single module body
//! covers both kinds with different finishes.
//!
//! Anything the user already declared with the same name on the same origin
//! wins — `find_material_scoped` resolves the user's version first.

use std::path::Path;

use mogen_core::{AlphaMode, Material, SceneGraph};

/// Stable material name used on the wrapping opening group for entrance
/// (exterior) door openings. Exposed so the emit pass binds the same name
/// `ensure_opening_defaults` declares.
pub(super) const EXT_DOOR_MAT: &str = "ext_door";
/// Material bound onto interior door opening groups.
pub(super) const INT_DOOR_MAT: &str = "int_door";
/// Material bound onto window opening groups so the pane inherits glass.
pub(super) const WINDOW_GLASS_MAT: &str = "window_glass";
/// Material bound onto skylight opening groups so the pane inherits glass.
/// Reuses `WINDOW_GLASS_MAT` — skylights and windows share the same pane
/// finish unless the user overrides.
pub(super) const SKYLIGHT_GLASS_MAT: &str = WINDOW_GLASS_MAT;

/// Stamp the default opening materials onto the graph so the stdlib
/// door / window / skylight modules can bind to them. Existing materials
/// with the same name + origin are left untouched, which lets a user
/// override any of these by declaring their own `material "door_frame" (...)`
/// (etc.) before the `building` node.
pub(super) fn ensure_opening_defaults(
    graph: &mut SceneGraph,
    origin: Option<&Path>,
) {
    // (name, factory). The factory is called only when the material isn't
    // already declared, so each closure builds a fresh Material that we
    // can stamp the canonical name + origin onto.
    let defaults: &[(&str, fn() -> Material)] = &[
        // Painted trim around door openings. Matt off-white so it reads as
        // a separate band from both the wall plaster and the door slab.
        ("door_frame", || {
            let mut m = Material::new("door_frame");
            m.base_color = [0.92, 0.91, 0.88, 1.0];
            m.roughness = 0.6;
            m
        }),
        // Rich dark wood for entrance/front doors — heavier and more
        // saturated than interior slabs so the main entry reads at a
        // glance even on the smallest floorplate.
        (EXT_DOOR_MAT, || {
            let mut m = Material::new(EXT_DOOR_MAT);
            m.base_color = [0.40, 0.22, 0.10, 1.0];
            m.roughness = 0.55;
            m
        }),
        // Medium wood for interior doors. Lighter and warmer than the
        // entrance so the two kinds visibly differ in the same scene.
        (INT_DOOR_MAT, || {
            let mut m = Material::new(INT_DOOR_MAT);
            m.base_color = [0.62, 0.45, 0.28, 1.0];
            m.roughness = 0.7;
            m
        }),
        // White vinyl-style window frame. Slightly glossier than door
        // trim because painted aluminium / vinyl returns more specular.
        ("window_frame", || {
            let mut m = Material::new("window_frame");
            m.base_color = [0.95, 0.95, 0.95, 1.0];
            m.roughness = 0.4;
            m
        }),
        // Translucent blue glass for window + skylight panes. Blend +
        // transmission so the pane reads as see-through in glTF viewers
        // that honour KHR_materials_transmission; the alpha keeps a
        // visible tint in renderers that don't.
        (WINDOW_GLASS_MAT, || {
            let mut m = Material::new(WINDOW_GLASS_MAT);
            m.base_color = [0.72, 0.86, 0.95, 0.35];
            m.roughness = 0.05;
            m.alpha_mode = AlphaMode::Blend;
            m.transmission = 1.0;
            m.double_sided = true;
            m
        }),
        // Dark anodised-metal frame for skylights — distinct from the
        // white window trim so a roof skylight doesn't read as a window
        // when seen alongside one.
        ("skylight_frame", || {
            let mut m = Material::new("skylight_frame");
            m.base_color = [0.25, 0.25, 0.27, 1.0];
            m.roughness = 0.4;
            m.metallic = 0.7;
            m
        }),
    ];
    for (name, factory) in defaults {
        if graph.find_material_scoped(name, origin).is_some() {
            continue;
        }
        let mut mat = factory();
        mat.origin = origin.map(|p| p.to_path_buf());
        graph.add_material(mat);
    }
}
