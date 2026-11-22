//! Rust side of the `mogen` Python package.
//!
//! The editing model is [`mogen_dsl::syntax`] (lossless parse/print); this
//! crate adds the compile entry points the Python layer calls, mirroring
//! `mogen build` stage for stage so a script and the CLI produce
//! byte-identical output. The PyO3 module lives behind the `python` feature
//! so workspace builds don't need a Python toolchain.

use std::path::Path;

use anyhow::Result;
use mogen_core::{has_errors, Diagnostic, SceneGraph};
use mogen_dsl::module::FsLoader;
use mogen_export::ExportOptions;

#[cfg(feature = "python")]
mod py;

/// Output container for [`compile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Glb,
    Fbx,
}

/// Result of [`check`] / [`compile`]: the scene or exported bytes when the
/// source built, plus every diagnostic (warnings included) either way.
pub struct Outcome<T> {
    pub output: Option<T>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse, validate and lower `src` without exporting — the same stages and
/// diagnostics as `mogen build`. `base_dir` resolves relative `import`s and
/// texture paths (normally the `.mog` file's directory).
pub fn check(src: &str, base_dir: Option<&Path>) -> Outcome<SceneGraph> {
    let fail = |diagnostics| Outcome { output: None, diagnostics };
    let ast = match mogen_dsl::parse(src) {
        Ok(a) => a,
        Err(e) => return fail(vec![Diagnostic::error("E0001", format!("parse: {e:#}"))]),
    };
    let mut diags = mogen_validate::validate_ast_with_source(&ast, base_dir);
    if has_errors(&diags) {
        return fail(diags);
    }
    let mut loader = FsLoader::new();
    let mut scene = match mogen_dsl::lower_with_loader(&ast, base_dir, &mut loader) {
        Ok(s) => s,
        Err(e) => {
            diags.push(Diagnostic::error("E0701", format!("lowering: {e:#}")));
            return fail(diags);
        }
    };
    diags.extend(mogen_validate::validate_graph(&scene));
    if has_errors(&diags) {
        return fail(diags);
    }
    if let Some(base) = base_dir {
        scene.resolve_texture_paths(base);
    }
    Outcome { output: Some(scene), diagnostics: diags }
}

/// [`check`], then export with default options (what `mogen build` writes).
pub fn compile(src: &str, base_dir: Option<&Path>, format: Format) -> Outcome<Vec<u8>> {
    let Outcome { output, mut diagnostics } = check(src, base_dir);
    let Some(scene) = output else { return Outcome { output: None, diagnostics } };
    let opts = ExportOptions::default();
    let bytes = match format {
        Format::Glb => mogen_export::build_glb_with_options(&scene, &opts, |_| {}),
        Format::Fbx => mogen_export::build_fbx_with_options(&scene, &opts, |_| {}),
    };
    match bytes {
        Ok(b) => Outcome { output: Some(b), diagnostics },
        Err(e) => {
            diagnostics.push(Diagnostic::error("E0801", format!("export: {e:#}")));
            Outcome { output: None, diagnostics }
        }
    }
}

fn is_binary(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mogb"))
}

/// Read a `.mog` (text) or `.mogb` (binary container) file as source text.
pub fn read_source(path: &Path) -> Result<String> {
    if is_binary(path) {
        mogen_binary::unpack_to_source(&std::fs::read(path)?)
    } else {
        Ok(std::fs::read_to_string(path)?)
    }
}

/// Write source text to `path`, packing it as MOGB for a `.mogb` target.
pub fn write_source(path: &Path, src: &str) -> Result<()> {
    let bytes = if is_binary(path) { mogen_binary::pack_source(src)? } else { src.as_bytes().to_vec() };
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_dsl::syntax;

    fn example(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(rel)
    }

    /// The Python path (syntax parse → print → compile) must write exactly
    /// what `mogen build` writes for the same file.
    #[test]
    fn reprinted_source_compiles_to_the_same_glb() {
        let path = example("furniture/chair.mog");
        let src = std::fs::read_to_string(&path).unwrap();
        let reprinted = syntax::print(&syntax::parse(&src).unwrap()).text;
        let dir = path.parent();
        let a = compile(&src, dir, Format::Glb).output.unwrap();
        let b = compile(&reprinted, dir, Format::Glb).output.unwrap();
        assert_eq!(a, b);
        let mut scene = mogen_dsl::lower_with_loader(
            &mogen_dsl::parse(&src).unwrap(),
            dir,
            &mut FsLoader::new(),
        )
        .unwrap();
        scene.resolve_texture_paths(dir.unwrap());
        let cli = mogen_export::build_glb_with_options(&scene, &ExportOptions::default(), |_| {})
            .unwrap();
        assert_eq!(a, cli);
    }

    #[test]
    fn errors_come_back_as_diagnostics() {
        let out = compile("scene { box (size=) }", None, Format::Glb);
        assert!(out.output.is_none());
        assert_eq!(out.diagnostics[0].code, "E0001");
        let out = compile("scene { box (mat=\"missing\") }", None, Format::Glb);
        assert!(out.output.is_none());
        assert!(out.diagnostics.iter().any(|d| d.span.is_some()));
    }

    #[test]
    fn mogb_round_trip() {
        let dir = std::env::temp_dir().join(format!("mogen-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.mogb");
        let src = "scene {\n  box (size=1)\n}\n";
        write_source(&p, src).unwrap();
        let back = read_source(&p).unwrap();
        crate::check(&back, None).output.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
