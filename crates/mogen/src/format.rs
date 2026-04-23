use std::fs;
use std::path::Path;
use std::time::Duration;

/// Summary shown after `build` / `generate` / `modify`. Compact, single-line,
/// dot-separated so it reads well in a terminal: prioritises the stats users
/// care about when iterating on prompts (geometry size, structural counts,
/// file size, time).
pub(crate) fn print_build_summary(out: &Path, scene: &mogen_core::SceneGraph, elapsed: Duration) {
    let mut mesh_count = 0usize;
    let mut tri_count = 0usize;
    let mut vert_count = 0usize;
    for n in &scene.nodes {
        if let Some(m) = &n.mesh {
            mesh_count += 1;
            tri_count += m.indices.len() / 3;
            vert_count += m.positions.len();
        }
    }
    let size = fs::metadata(out).map(|m| m.len()).ok();

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{} tris", format_count(tri_count)));
    parts.push(format!("{} verts", format_count(vert_count)));
    parts.push(format!("{} nodes", scene.nodes.len()));
    if mesh_count != scene.nodes.len() {
        parts.push(format!("{mesh_count} meshes"));
    }
    parts.push(format!("{} materials", scene.materials.len()));
    if !scene.skins.is_empty() {
        parts.push(format!("{} skins", scene.skins.len()));
    }
    if !scene.clips.is_empty() {
        parts.push(format!("{} clips", scene.clips.len()));
    }
    if !scene.joints.is_empty() {
        parts.push(format!("{} joints", scene.joints.len()));
    }
    if let Some(bytes) = size {
        parts.push(format_bytes(bytes));
    }
    parts.push(format_duration(elapsed));

    println!("✓ {}  ·  {}", out.display(), parts.join("  ·  "));
}

/// Human counts: "2.1k", "1.3M", plain integer below 1000.
pub(crate) fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

pub(crate) fn format_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let n_f = n as f64;
    if n_f >= MIB {
        format!("{:.2} MiB", n_f / MIB)
    } else if n_f >= KIB {
        format!("{:.1} KiB", n_f / KIB)
    } else {
        format!("{n} B")
    }
}

pub(crate) fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let m = (secs / 60.0) as u64;
        let s = secs - (m as f64 * 60.0);
        format!("{m}m{s:02.0}s")
    }
}

pub(crate) fn print_gltf_summary(v: &serde_json::Value) {
    let count = |key: &str| v.get(key).and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0);
    println!(
        "glTF: nodes={} meshes={} materials={} accessors={} bufferViews={} skins={}",
        count("nodes"),
        count("meshes"),
        count("materials"),
        count("accessors"),
        count("bufferViews"),
        count("skins")
    );
    if let Some(scenes) = v.get("scenes").and_then(|s| s.as_array()) {
        for (i, s) in scenes.iter().enumerate() {
            if let Some(roots) = s.get("nodes").and_then(|n| n.as_array()) {
                let r: Vec<String> = roots.iter().map(|n| n.to_string()).collect();
                println!("scene[{i}] roots=[{}]", r.join(","));
            }
        }
    }
    if let Some(nodes) = v.get("nodes").and_then(|n| n.as_array()) {
        for (i, n) in nodes.iter().enumerate() {
            let name = n.get("name").and_then(|s| s.as_str()).unwrap_or("?");
            let mesh = n.get("mesh").and_then(|m| m.as_u64());
            let skin = n.get("skin").and_then(|s| s.as_u64());
            let children = n
                .get("children")
                .and_then(|c| c.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            println!("  node[{i}] {name:?} mesh={mesh:?} skin={skin:?} children={children}");
        }
    }
    if let Some(skins) = v.get("skins").and_then(|s| s.as_array()) {
        for (i, s) in skins.iter().enumerate() {
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let joints = s
                .get("joints")
                .and_then(|j| j.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let root = s.get("skeleton").and_then(|r| r.as_u64());
            let ibm = s.get("inverseBindMatrices").and_then(|i| i.as_u64());
            println!("  skin[{i}] {name:?} joints={joints} skeleton={root:?} ibm_accessor={ibm:?}");
        }
    }
    if let Some(mats) = v.get("materials").and_then(|m| m.as_array()) {
        for (i, m) in mats.iter().enumerate() {
            let name = m.get("name").and_then(|s| s.as_str()).unwrap_or("?");
            let color = m
                .get("pbrMetallicRoughness")
                .and_then(|p| p.get("baseColorFactor"));
            println!("  material[{i}] {name:?} color={color:?}");
        }
    }
}
