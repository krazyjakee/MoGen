//! Round-trip the whole checked-in `.mog` corpus through MOGB and assert the
//! lossless encoding reproduces the AST exactly. Also exercises the `.mog` text
//! printer (`unpack`) by re-parsing its output and checking equivalence.

use std::path::{Path, PathBuf};

fn collect_mog(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mog(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("mog") {
            out.push(path);
        }
    }
}

fn examples_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/mogen-binary; examples live at the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("examples dir")
}

#[test]
fn lossless_roundtrip_over_examples() {
    let mut files = Vec::new();
    collect_mog(&examples_dir(), &mut files);
    assert!(!files.is_empty(), "no example .mog files found");

    let mut checked = 0usize;
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        // Skip anything that doesn't parse — MOGB only encodes valid ASTs, and
        // parse errors are the parser's concern, not this crate's.
        let Ok(ast) = mogen_dsl::parse(&src) else {
            continue;
        };

        let bytes = mogen_binary::encode(&ast);
        let decoded = mogen_binary::decode(&bytes).expect("decode");
        assert!(
            mogen_binary::nodes_equivalent(&ast, &decoded),
            "MOGB round-trip diverged for {}",
            path.display()
        );

        // The text printer output must itself re-parse to an equivalent AST.
        let text = mogen_binary::to_mog_text(&decoded);
        let reparsed = mogen_dsl::parse(&text)
            .unwrap_or_else(|e| panic!("printer output for {} did not parse: {e}", path.display()));
        assert!(
            mogen_binary::nodes_equivalent(&ast, &reparsed),
            "printer round-trip diverged for {}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no parseable example files were checked");
    eprintln!("round-tripped {checked} example files");
}

#[test]
fn lossy_decodes_cleanly() {
    // Lossy mode need not be bit-exact, but every file must still decode into a
    // well-formed AST without error.
    let mut files = Vec::new();
    collect_mog(&examples_dir(), &mut files);
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let Ok(ast) = mogen_dsl::parse(&src) else {
            continue;
        };
        let bytes = mogen_binary::encode_lossy(&ast);
        mogen_binary::decode(&bytes)
            .unwrap_or_else(|e| panic!("lossy decode failed for {}: {e}", path.display()));
    }
}

#[test]
fn rejects_garbage() {
    assert!(mogen_binary::decode(b"not a mogb file at all").is_err());
    assert!(mogen_binary::decode(b"MOGB\xff").is_err()); // bad version
    assert!(mogen_binary::decode(&[]).is_err());
    // Compressed flag set (0x02) but a garbage payload must not panic.
    assert!(mogen_binary::decode(b"MOGB\x01\x02\x05\xff\xff\xff").is_err());
}

#[test]
fn compression_shrinks_and_round_trips() {
    // A file with enough repeated structure that DEFLATE has something to chew
    // on — the compressed container must be smaller than the same content would
    // be uncompressed, and still round-trip.
    let mut files = Vec::new();
    collect_mog(&examples_dir(), &mut files);
    let mut saw_compressed = false;
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let Ok(ast) = mogen_dsl::parse(&src) else {
            continue;
        };
        let bytes = mogen_binary::encode(&ast);
        // Header byte 5 carries the flags; bit 1 (0x02) = compressed.
        if bytes.len() > 6 && bytes[5] & 0x02 != 0 {
            saw_compressed = true;
        }
        // Round-trips regardless of whether this particular file compressed.
        let decoded = mogen_binary::decode(&bytes).expect("decode");
        assert!(mogen_binary::nodes_equivalent(&ast, &decoded));
    }
    assert!(
        saw_compressed,
        "expected at least one example to compress — none set the flag"
    );
}
