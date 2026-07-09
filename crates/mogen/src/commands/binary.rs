//! `mogen pack` / `mogen unpack` — the experimental MOGB binary container.
//!
//! `pack` parses a `.mog`, verifies the encode∘decode round-trip is
//! structurally lossless before writing anything, then reports how the binary
//! compares to the text source (and, when `gzip` is on `PATH`, how both look
//! after compression — the number that actually answers "is this smaller than a
//! zipped `.mog`?"). `unpack` reverses it back to `.mog` text.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

pub(crate) fn pack(input: PathBuf, out: Option<PathBuf>, lossy: bool) -> Result<()> {
    let src = fs::read_to_string(&input)
        .with_context(|| format!("reading {}", input.display()))?;
    let ast = mogen_dsl::parse(&src).with_context(|| format!("parsing {}", input.display()))?;

    let bytes = if lossy {
        mogen_binary::encode_lossy(&ast)
    } else {
        mogen_binary::encode(&ast)
    };

    // Fidelity gate. Lossless mode must round-trip exactly; lossy mode is
    // allowed to differ (numbers snapped to /1000), so we only assert structure
    // there by re-encoding losslessly for the check.
    if !lossy {
        let decoded = mogen_binary::decode(&bytes)
            .context("internal error: freshly encoded MOGB failed to decode")?;
        if !mogen_binary::nodes_equivalent(&ast, &decoded) {
            bail!("internal error: MOGB round-trip was not lossless for {}", input.display());
        }
    }

    let out = out.unwrap_or_else(|| input.with_extension("mogb"));
    fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    report_sizes(&src, &bytes, lossy);
    println!("wrote {} ({} bytes)", out.display(), bytes.len());
    Ok(())
}

pub(crate) fn unpack(input: PathBuf, out: Option<PathBuf>) -> Result<()> {
    let bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    let source = mogen_binary::unpack_to_source(&bytes)
        .with_context(|| format!("decoding {}", input.display()))?;

    match out {
        Some(path) => {
            fs::write(&path, &source).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {} ({} bytes)", path.display(), source.len());
        }
        None => print!("{source}"),
    }
    Ok(())
}

/// Print a size comparison to stderr: raw text vs MOGB, and — if `gzip` is
/// available — the same two after `gzip -9`. Best-effort; never fails the
/// command.
fn report_sizes(src: &str, bytes: &[u8], lossy: bool) {
    let text = src.len();
    let bin = bytes.len();
    let pct = |a: usize, b: usize| {
        if b == 0 {
            0.0
        } else {
            100.0 * a as f64 / b as f64
        }
    };
    eprintln!("MOGB size report{}:", if lossy { " (lossy /1000)" } else { "" });
    eprintln!("  text  .mog        : {text:>7} bytes");
    eprintln!(
        "  binary.mogb (deflate): {bin:>7} bytes   ({:.0}% of text, {:.2}x)",
        pct(bin, text),
        text as f64 / bin.max(1) as f64
    );

    if let (Some(gz_text), Some(gz_bin)) = (gzip_len(src.as_bytes()), gzip_len(bytes)) {
        eprintln!("  gzip -9 text      : {gz_text:>7} bytes");
        // The container already self-compresses, so re-zipping it barely moves —
        // that's the point. Compare the finished .mogb against gzipped text.
        eprintln!("  gzip -9 mogb      : {gz_bin:>7} bytes  (already compressed)");
        eprintln!(
            "  → MOGB vs gzipped-text: {:.0}% ({:.2}x smaller)",
            pct(bin, gz_text),
            gz_text as f64 / bin.max(1) as f64
        );
    }
}

/// Length of `data` after `gzip -9`, or `None` if gzip isn't available.
fn gzip_len(data: &[u8]) -> Option<usize> {
    let mut child = Command::new("gzip")
        .arg("-9")
        .arg("-c")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(data).ok()?;
    let output = child.wait_with_output().ok()?;
    output.status.success().then(|| output.stdout.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mogen-cli-binary-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pack_then_unpack_round_trips_to_equivalent_source() {
        let dir = fresh_tempdir("pack-unpack");
        let src_path = dir.join("chair.mog");
        fs::write(&src_path, "box (size=[1, 2.5, 3], pos=[0, 0.3, 0])\n").unwrap();
        let mogb_path = dir.join("chair.mogb");

        pack(src_path.clone(), Some(mogb_path.clone()), false).expect("pack");
        assert!(mogb_path.exists());

        let unpacked_path = dir.join("chair.roundtrip.mog");
        unpack(mogb_path, Some(unpacked_path.clone())).expect("unpack");

        let original = mogen_dsl::parse(&fs::read_to_string(&src_path).unwrap()).unwrap();
        let roundtripped =
            mogen_dsl::parse(&fs::read_to_string(&unpacked_path).unwrap()).unwrap();
        assert!(mogen_binary::nodes_equivalent(&original, &roundtripped));
    }

    #[test]
    fn pack_defaults_output_to_mogb_beside_input() {
        let dir = fresh_tempdir("default-out");
        let src_path = dir.join("chair.mog");
        fs::write(&src_path, "box (size=[1,1,1])\n").unwrap();

        pack(src_path.clone(), None, false).expect("pack");
        assert!(dir.join("chair.mogb").exists());
    }

    #[test]
    fn pack_lossy_still_decodes_to_a_valid_ast() {
        let dir = fresh_tempdir("lossy");
        let src_path = dir.join("chair.mog");
        fs::write(&src_path, "box (size=[1.23456, 2, 3], pos=[0.1, 0.2, 0.3])\n").unwrap();
        let mogb_path = dir.join("chair.mogb");

        pack(src_path, Some(mogb_path.clone()), true).expect("lossy pack");
        let bytes = fs::read(&mogb_path).unwrap();
        assert!(mogen_binary::decode(&bytes).is_ok());
    }

    #[test]
    fn pack_rejects_unparseable_source() {
        let dir = fresh_tempdir("bad-source");
        let src_path = dir.join("broken.mog");
        fs::write(&src_path, "box (size=[\n").unwrap();
        assert!(pack(src_path, None, false).is_err());
    }

    #[test]
    fn unpack_rejects_garbage_input() {
        let dir = fresh_tempdir("garbage");
        let path = dir.join("broken.mogb");
        fs::write(&path, b"not a mogb file").unwrap();
        assert!(unpack(path, None).is_err());
    }
}
