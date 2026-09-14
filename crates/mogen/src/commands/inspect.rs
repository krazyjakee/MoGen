use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use crate::format::print_gltf_summary;

pub(crate) fn parse_cmd(input: PathBuf) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mogen_dsl::parse(&src)?;
    println!("{:#?}", ast);
    Ok(())
}

pub(crate) fn check(input: PathBuf, json: bool) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mogen_dsl::parse(&src)?;
    let mut diags = mogen_validate::validate_ast_with_source(&ast, input.parent());
    let filename = input.to_string_lossy().to_string();
    let scene = if mogen_core::has_errors(&diags) {
        None
    } else {
        match mogen_dsl::lower_with_source(&ast, input.parent()) {
            Ok(g) => {
                diags.extend(mogen_validate::validate_graph(&g));
                Some(g)
            }
            Err(e) => {
                diags.push(mogen_core::Diagnostic::error(
                    "E0701",
                    format!("lowering error: {e}"),
                ));
                None
            }
        }
    };

    if json {
        print!("{}", mogen_validate::render_json(&filename, &diags));
    } else {
        mogen_validate::render_human(&filename, &src, &diags);
    }
    if mogen_core::has_errors(&diags) {
        std::process::exit(1);
    }
    if !json {
        let scene = scene.expect("no errors should imply lowered scene");
        println!(
            "ok: {} ({} nodes, {} materials, {} diagnostic{})",
            input.display(),
            scene.nodes.len(),
            scene.materials.len(),
            diags.len(),
            if diags.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

pub(crate) fn dump_scene(input: PathBuf, as_json: bool) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mogen_dsl::parse(&src)?;
    let scene = mogen_dsl::lower_with_source(&ast, input.parent())?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&scene)?);
    } else {
        println!("{:#?}", scene);
    }
    Ok(())
}

pub(crate) fn inspect(input: PathBuf) -> Result<()> {
    if input.extension().is_some_and(|e| e == "mog") {
        let input = input.canonicalize()?;
        let workspace = mogen_llm::session::ModelingWorkspace::new(
            fs::read_to_string(&input)?,
            input.parent().map(|p| p.to_owned()),
            Vec::new(),
            None,
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&workspace.inspect(None)?)?
        );
        return Ok(());
    }
    let data = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    if data.len() < 12 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != 0x46546C67 {
        return Err(anyhow!("not a GLB file: {}", input.display()));
    }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let total = u32::from_le_bytes(data[8..12].try_into().unwrap());
    println!("GLB version={version} total_bytes={total}");

    let mut off = 12usize;
    while off + 8 <= data.len() {
        let chunk_len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        let chunk_type = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
        off += 8;
        match chunk_type {
            0x4E4F534A => {
                let txt = std::str::from_utf8(&data[off..off + chunk_len])
                    .unwrap_or("")
                    .trim_end_matches(' ');
                let v: serde_json::Value = serde_json::from_str(txt)?;
                print_gltf_summary(&v);
            }
            0x004E4942 => {
                println!("BIN chunk: {chunk_len} bytes");
            }
            t => println!("unknown chunk 0x{t:08X} ({chunk_len} bytes)"),
        }
        off += chunk_len;
    }
    Ok(())
}

pub(crate) fn measure(
    input: PathBuf,
    first: String,
    second: String,
    tolerance: f64,
    max_work: usize,
) -> Result<()> {
    let input = input.canonicalize()?;
    let source = fs::read_to_string(&input)?;
    let workspace = mogen_llm::session::ModelingWorkspace::new(
        source,
        input.parent().map(|p| p.to_owned()),
        Vec::new(),
        None,
    )?;
    let revision = workspace.revision()?;
    let result = workspace.measure(&revision, &first, &second, Some(tolerance), Some(max_work))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
