//! Read-only "Connections" navigator for the selected node.
//!
//! `.mog`'s real structure is two graphs layered on the source: the
//! containment tree (visible from indentation) and a *symbolic reference
//! graph* — `attach`, `conform`, `material=`, joints, clip tracks, skin
//! binding — that is invisible in both the text and the rest of the
//! inspector. This subsection surfaces that second graph for whichever node
//! is selected, with every node↔node link clickable so the user can walk
//! relationships the way they'd click around a node editor.
//!
//! It is strictly read-only: a click only re-targets the 3D/inspector
//! selection (the caller calls `set_primary_selection`). Nothing here mutates
//! source, so there is no round-trip / span risk — the design constraint that
//! ruled the rest of the visual-editor work.

use eframe::egui;
use mogen_core::{NodeId, SceneGraph, SceneNode};

/// Render the section. Returns the node the user clicked to navigate to, if
/// any (the caller applies the selection change so borrow scopes stay clean).
pub(super) fn render(
    ui: &mut egui::Ui,
    scene: &SceneGraph,
    sel: NodeId,
    node: &SceneNode,
) -> Option<NodeId> {
    let mut nav: Option<NodeId> = None;

    // --- Ancestor breadcrumb -------------------------------------------------
    // Walk parent links to the root so the user can jump up a deep hierarchy
    // (the leg → hinge → knee → capsule nesting in rigged characters is the
    // motivating pain) without leaving the inspector.
    let mut chain: Vec<NodeId> = Vec::new();
    let mut cur = node.parent;
    while let Some(p) = cur {
        chain.push(p);
        cur = scene.nodes.get(p.0 as usize).and_then(|n| n.parent);
    }
    chain.reverse();

    let has_children = !node.children.is_empty();
    let attach_out = node.attach_binding.as_ref().map(|b| (b.parent, b.socket.clone()));
    let attach_in: Vec<NodeId> = scene
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.attach_binding.as_ref().map(|b| b.parent) == Some(sel))
        .map(|(i, _)| NodeId(i as u32))
        .collect();
    let conform_target = node.conform_binding.as_ref().map(|c| c.target());

    let joints_driving: Vec<&mogen_core::Joint> =
        scene.joints.iter().filter(|j| j.pivot == sel).collect();
    let clips_targeting: Vec<(&str, usize)> = scene
        .clips
        .iter()
        .filter_map(|c| {
            let n = c.tracks.iter().filter(|t| t.node == sel).count();
            (n > 0).then_some((c.name.as_str(), n))
        })
        .collect();
    let skin_name = node
        .skin
        .and_then(|s| scene.skins.get(s.0 as usize))
        .map(|s| s.name.clone());
    let bone_of: Vec<&str> = scene
        .skins
        .iter()
        .filter(|s| s.joints.contains(&sel))
        .map(|s| s.name.as_str())
        .collect();
    let skeleton_root_of: Vec<&str> = scene
        .skins
        .iter()
        .filter(|s| s.skeleton_root == Some(sel))
        .map(|s| s.name.as_str())
        .collect();
    let material_name = node
        .material
        .and_then(|m| scene.materials.get(m.0 as usize))
        .map(|m| m.name.clone());

    let nothing = chain.is_empty()
        && !has_children
        && attach_out.is_none()
        && attach_in.is_empty()
        && conform_target.is_none()
        && joints_driving.is_empty()
        && clips_targeting.is_empty()
        && skin_name.is_none()
        && bone_of.is_empty()
        && skeleton_root_of.is_empty()
        && material_name.is_none();
    if nothing {
        return None;
    }

    // Default-open only when there is a node↔node link worth navigating;
    // pure leaf nodes with just a parent breadcrumb stay folded so the
    // section doesn't push the transform grid down for every selection.
    let default_open = attach_out.is_some()
        || !attach_in.is_empty()
        || conform_target.is_some()
        || has_children;

    ui.add_space(8.0);
    ui.separator();
    egui::CollapsingHeader::new("Connections")
        .id_salt(("inspector_connections_nav", sel.0))
        .default_open(default_open)
        .show(ui, |ui| {
            let name_of = |id: NodeId| -> String {
                scene
                    .nodes
                    .get(id.0 as usize)
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| "(?)".to_string())
            };

            // Breadcrumb: root › … › parent
            if !chain.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Path:").weak());
                    for (idx, id) in chain.iter().enumerate() {
                        if idx > 0 {
                            ui.label(egui::RichText::new("›").weak());
                        }
                        if ui
                            .link(egui::RichText::new(name_of(*id)).monospace())
                            .on_hover_text("Select this ancestor")
                            .clicked()
                        {
                            nav = Some(*id);
                        }
                    }
                });
            }

            // Children
            if has_children {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Children ({}):", node.children.len()))
                            .weak(),
                    );
                    for id in &node.children {
                        if ui
                            .link(egui::RichText::new(name_of(*id)).monospace())
                            .on_hover_text("Select this child")
                            .clicked()
                        {
                            nav = Some(*id);
                        }
                    }
                });
            }

            // Attach (outgoing) — this node is rigidly placed on a parent's
            // socket. This is the single most useful link to surface: attach
            // reparents at lower time, so the authored relationship is
            // otherwise invisible in the selected node's own attributes.
            if let Some((parent, socket)) = &attach_out {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("⚓ attached to").weak());
                    if ui
                        .link(egui::RichText::new(name_of(*parent)).monospace())
                        .on_hover_text("Select the attach parent")
                        .clicked()
                    {
                        nav = Some(*parent);
                    }
                    ui.label(egui::RichText::new(format!("· socket “{socket}”")).weak());
                });
            }

            // Attach (incoming) — other nodes plugged onto this one.
            if !attach_in.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("⚓ holds ({}):", attach_in.len())).weak(),
                    );
                    for id in &attach_in {
                        if ui
                            .link(egui::RichText::new(name_of(*id)).monospace())
                            .on_hover_text("Select the attached child")
                            .clicked()
                        {
                            nav = Some(*id);
                        }
                    }
                });
            }

            // Conform — this node's mesh was moulded onto a target surface.
            if let Some(t) = conform_target {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("conformed to").weak());
                    if ui
                        .link(egui::RichText::new(name_of(t)).monospace())
                        .on_hover_text("Select the conform target")
                        .clicked()
                    {
                        nav = Some(t);
                    }
                });
            }

            // Material — informational; the picker below edits it. Shown here
            // so the full relationship set reads in one place.
            if let Some(m) = &material_name {
                ui.label(
                    egui::RichText::new(format!("material: {m}"))
                        .weak()
                        .monospace(),
                );
            }

            // Animation rig — joints/clips/skin are not scene nodes, so these
            // are read-only context rather than navigation targets.
            for j in &joints_driving {
                ui.label(
                    egui::RichText::new(format!(
                        "🦴 driven by joint “{}” ({:?})",
                        j.name, j.kind
                    ))
                    .weak(),
                );
            }
            for (clip, n) in &clips_targeting {
                ui.label(
                    egui::RichText::new(format!("▶ animated by clip “{clip}” ({n} tracks)"))
                        .weak(),
                );
            }
            if let Some(s) = &skin_name {
                ui.label(egui::RichText::new(format!("skinned to “{s}”")).weak());
            }
            for s in &bone_of {
                ui.label(egui::RichText::new(format!("bone in skin “{s}”")).weak());
            }
            for s in &skeleton_root_of {
                ui.label(egui::RichText::new(format!("skeleton root of “{s}”")).weak());
            }
        });

    nav
}
