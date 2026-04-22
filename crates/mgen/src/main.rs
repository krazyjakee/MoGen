use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use mgen_core::{Diagnostic, Severity};
use mgen_llm::gemini::{GeminiClient, GenerateConfig, DEFAULT_MODEL};
use mgen_llm::{
    default_cache_path, embed_seed_header, generate_with_repair, parse_seed_header,
    resolve_or_create_cache, system_instruction, RepairConfig, StdlibIndex, ThinkingLevel,
    DEFAULT_TTL_SECONDS,
};

/// CLI-facing mirror of [`ThinkingLevel`]. Kept separate so we don't leak
/// `clap::ValueEnum` into the `mgen-llm` library crate.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ThinkingArg {
    Low,
    Medium,
    High,
    Xhigh,
}

impl From<ThinkingArg> for ThinkingLevel {
    fn from(a: ThinkingArg) -> Self {
        match a {
            ThinkingArg::Low => ThinkingLevel::Low,
            ThinkingArg::Medium => ThinkingLevel::Medium,
            ThinkingArg::High => ThinkingLevel::High,
            ThinkingArg::Xhigh => ThinkingLevel::XHigh,
        }
    }
}

/// Rotating "still working" lines shown after a Gemini call has been running
/// for more than 10s. They're deliberately vague — we don't actually know
/// what the model is doing, we just want the wait to feel less dead.
const GEMINI_FLAVORS: &[&str] = &[
    "still thinking",
    "reasoning about geometry",
    "planning the scene",
    "picking materials",
    "wiring connectors",
    "working it out",
];

struct SpinnerState {
    base: String,
    since: Instant,
    flavors: &'static [&'static str],
}

#[derive(Clone)]
struct SpinnerHandle {
    pb: ProgressBar,
    state: Arc<Mutex<SpinnerState>>,
}

impl SpinnerHandle {
    fn set_message(&self, msg: impl Into<String>) {
        let msg = msg.into();
        {
            let mut s = self.state.lock().expect("spinner state mutex poisoned");
            s.base = msg.clone();
            s.since = Instant::now();
        }
        self.pb.set_message(msg);
    }
}

/// Terminal spinner with optional rotating "flavor text" on long waits.
/// Falls back silently to a no-op when stderr isn't a TTY (indicatif detects
/// this automatically), so piped/logged output stays clean.
struct Spinner {
    handle: SpinnerHandle,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Spinner {
    fn new(initial: &str, flavors: &'static [&'static str]) -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan.bold} [{elapsed:>3}] {msg}")
                .expect("static template")
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
        );
        pb.enable_steady_tick(Duration::from_millis(80));
        pb.set_message(initial.to_string());
        let state = Arc::new(Mutex::new(SpinnerState {
            base: initial.to_string(),
            since: Instant::now(),
            flavors,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let join = if flavors.is_empty() {
            None
        } else {
            let pb_t = pb.clone();
            let state_t = state.clone();
            let stop_t = stop.clone();
            Some(std::thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    if stop_t.load(Ordering::Relaxed) {
                        break;
                    }
                    let (base, elapsed, flavors) = {
                        let s = state_t.lock().expect("spinner state mutex poisoned");
                        (s.base.clone(), s.since.elapsed(), s.flavors)
                    };
                    if elapsed >= Duration::from_secs(10) && !flavors.is_empty() {
                        let idx = ((elapsed.as_secs() - 10) / 4) as usize % flavors.len();
                        pb_t.set_message(format!("{base}  ·  {}…", flavors[idx]));
                    }
                }
            }))
        };
        Spinner {
            handle: SpinnerHandle { pb, state },
            stop,
            join,
        }
    }

    fn handle(&self) -> SpinnerHandle {
        self.handle.clone()
    }

    fn set_message(&self, msg: impl Into<String>) {
        self.handle.set_message(msg);
    }

    fn finish_with_message(&mut self, msg: String) {
        self.stop_thread();
        self.handle.pb.finish_with_message(msg);
    }

    fn abandon_with_message(&mut self, msg: String) {
        self.stop_thread();
        self.handle.pb.abandon_with_message(msg);
    }

    fn stop_thread(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

/// Group error diagnostics by category (derived from the code prefix) so the
/// repair spinner can say "fixing 2 syntax, 1 attach" instead of just "3".
fn summarize_repair_errors(diags: &[Diagnostic]) -> String {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut total = 0usize;
    for d in diags.iter().filter(|d| matches!(d.severity, Severity::Error)) {
        *counts.entry(diag_category(&d.code)).or_insert(0) += 1;
        total += 1;
    }
    if total == 0 {
        return "0 errors".to_string();
    }
    let parts: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    parts.join(", ")
}

/// Maps diagnostic codes like "E0104" / "E1101" to a human-friendly category
/// name. Based on the two-digit prefix after the severity letter.
fn diag_category(code: &str) -> &'static str {
    let rest = match code.strip_prefix(|c: char| c.is_ascii_alphabetic()) {
        Some(r) => r,
        None => return "other",
    };
    let prefix: String = rest.chars().take(2).collect();
    match prefix.as_str() {
        "01" => "syntax",
        "02" => "material",
        "03" => "module",
        "04" => "animation",
        "05" => "skeleton",
        "06" => "attach",
        "07" => "lowering",
        "11" => "topology",
        _ => "other",
    }
}

#[derive(Parser)]
#[command(name = "mgen", about = "Procedural 3D model generator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compile a DSL file to GLB.
    Build {
        input: PathBuf,
        /// Output GLB path. Defaults to `<input>.glb` alongside the DSL file.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Parse a DSL file and print the AST.
    Parse { input: PathBuf },
    /// Validate a DSL file (semantic + reference checks). Exit non-zero on any error.
    Check {
        input: PathBuf,
        /// Emit diagnostics as line-delimited JSON.
        #[arg(long)]
        json: bool,
    },
    /// Lower a DSL file and print the scene graph as JSON.
    DumpScene {
        input: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Read a GLB and print its structure.
    Inspect { input: PathBuf },
    /// Generate a DSL file from a natural-language prompt via Gemini, then
    /// validate and compile it.
    Generate {
        /// Natural-language description of the asset, e.g. "a wooden stool".
        prompt: String,
        /// Output GLB path. Ignored in --dry-run mode.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where to stash the intermediate DSL. Defaults to sibling of `out`
        /// with a .mg extension; required with --dry-run if you want the DSL.
        #[arg(long)]
        dsl_out: Option<PathBuf>,
        /// Seed embedded in the DSL header for reproducibility. Randomized if omitted.
        #[arg(long)]
        seed: Option<u64>,
        /// Gemini model name.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        /// Print the generated DSL but skip compilation and GLB output.
        #[arg(long)]
        dry_run: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override GEMINI_API_KEY.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        /// If set, we skip uploading the grammar/stdlib reference on each call.
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `MGEN_CACHE_DIR`).
        /// By default, mgen creates and reuses a `cachedContents` resource so
        /// repeated calls skip re-uploading the grammar reference.
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Gemini default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. `low` is fastest (default); `xhigh`
        /// near-maximises quality but can take ~2 minutes on Pro.
        #[arg(long, value_enum, default_value_t = ThinkingArg::High)]
        thinking: ThinkingArg,
    },
    /// Modify an existing DSL file with a natural-language prompt via Gemini,
    /// then validate and recompile the GLB.
    Modify {
        /// Existing .mg file to modify.
        input: PathBuf,
        /// Natural-language description of the change, e.g. "make the legs taller".
        prompt: String,
        /// Output GLB path. Defaults to `<input>.glb`. Ignored with --dry-run.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where to write the modified DSL. Defaults to modifying `input` in place.
        #[arg(long)]
        dsl_out: Option<PathBuf>,
        /// Seed embedded in the DSL header. Defaults to the seed parsed from
        /// the input's header, or a random seed if absent.
        #[arg(long)]
        seed: Option<u64>,
        /// Gemini model name.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        /// Print the modified DSL but skip compilation and file writes.
        #[arg(long)]
        dry_run: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override GEMINI_API_KEY.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `--no-cache`
        /// on `generate` for details).
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Gemini default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. See `generate --thinking`.
        #[arg(long, value_enum, default_value_t = ThinkingArg::High)]
        thinking: ThinkingArg,
    },
    /// Run a suite of prompts through `generate` and report success rate and
    /// mean token cost. Does not write GLBs.
    Bench {
        /// File with one prompt per line; `#` starts a comment. Defaults to
        /// `benches/prompts.txt` in the project root.
        #[arg(long, default_value = "benches/prompts.txt")]
        prompts: PathBuf,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        #[arg(long)]
        budget_tokens: Option<u32>,
        #[arg(long)]
        api_key: Option<String>,
        /// Disable the automatic system-instruction cache.
        #[arg(long)]
        no_cache: bool,
        /// Cap on server-side reasoning. See `generate --thinking`.
        #[arg(long, value_enum, default_value_t = ThinkingArg::High)]
        thinking: ThinkingArg,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Build { input, out } => {
            let out = out.unwrap_or_else(|| input.with_extension("glb"));
            build(input, out)
        }
        Cmd::Parse { input } => parse_cmd(input),
        Cmd::Check { input, json } => check(input, json),
        Cmd::DumpScene { input, json } => dump_scene(input, json),
        Cmd::Inspect { input } => inspect(input),
        Cmd::Generate {
            prompt,
            out,
            dsl_out,
            seed,
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking,
        } => generate(GenerateArgs {
            prompt,
            out,
            dsl_out,
            seed,
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.into(),
        }),
        Cmd::Modify {
            input,
            prompt,
            out,
            dsl_out,
            seed,
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking,
        } => modify(ModifyArgs {
            input,
            prompt,
            out,
            dsl_out,
            seed,
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.into(),
        }),
        Cmd::Bench {
            prompts,
            model,
            max_repair_iters,
            budget_tokens,
            api_key,
            no_cache,
            thinking,
        } => bench(
            prompts,
            model,
            max_repair_iters,
            budget_tokens,
            api_key,
            no_cache,
            thinking.into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn build(input: PathBuf, out: PathBuf) -> Result<()> {
    let start = Instant::now();
    let label = input
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.to_string_lossy().into_owned());
    let filename = input.to_string_lossy().to_string();

    let mut spinner = Spinner::new(&format!("build {label}: reading"), &[]);

    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: couldn't read file"));
            return Err(anyhow::Error::new(e).context(format!("reading {}", input.display())));
        }
    };

    spinner.set_message(format!("build {label}: parsing"));
    let ast = match mgen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: parse error"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: validating DSL"));
    let diags = mgen_validate::validate_ast(&ast);
    if mgen_core::has_errors(&diags) {
        spinner.abandon_with_message(format!("build {label}: validation failed"));
        mgen_validate::render_human(&filename, &src, &diags);
        return Err(anyhow!("refusing to build: validation errors"));
    }
    if !diags.is_empty() {
        // Warnings — render them, but keep going. Render between spinner state
        // changes so codespan output doesn't collide with the live spinner line.
        spinner.handle().pb.suspend(|| {
            mgen_validate::render_human(&filename, &src, &diags);
        });
    }

    spinner.set_message(format!("build {label}: lowering scene"));
    let scene = match mgen_dsl::lower(&ast) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: lowering failed"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: checking scene graph"));
    let graph_diags = mgen_validate::validate_graph(&scene);
    if mgen_core::has_errors(&graph_diags) {
        spinner.abandon_with_message(format!("build {label}: graph validation failed"));
        mgen_validate::render_human(&filename, &src, &graph_diags);
        return Err(anyhow!("refusing to build: post-lowering validation errors"));
    }
    if !graph_diags.is_empty() {
        spinner.handle().pb.suspend(|| {
            mgen_validate::render_human(&filename, &src, &graph_diags);
        });
    }

    spinner.set_message(format!("build {label}: writing GLB"));
    if let Err(e) = mgen_export::write_glb(&scene, &out) {
        spinner.abandon_with_message(format!("build {label}: GLB export failed"));
        return Err(e);
    }

    let elapsed = start.elapsed();
    spinner.finish_with_message(format!(
        "build {label}: done in {}",
        format_duration(elapsed)
    ));
    print_build_summary(&out, &scene, elapsed);
    Ok(())
}

/// Summary shown after `build` / `generate` / `modify`. Compact, single-line,
/// dot-separated so it reads well in a terminal: prioritises the stats users
/// care about when iterating on prompts (geometry size, structural counts,
/// file size, time).
fn print_build_summary(out: &Path, scene: &mgen_core::SceneGraph, elapsed: Duration) {
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
fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_bytes(n: u64) -> String {
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

fn format_duration(d: Duration) -> String {
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

fn parse_cmd(input: PathBuf) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mgen_dsl::parse(&src)?;
    println!("{:#?}", ast);
    Ok(())
}

fn check(input: PathBuf, json: bool) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mgen_dsl::parse(&src)?;
    let mut diags = mgen_validate::validate_ast(&ast);
    let filename = input.to_string_lossy().to_string();
    let scene = if mgen_core::has_errors(&diags) {
        None
    } else {
        match mgen_dsl::lower(&ast) {
            Ok(g) => {
                diags.extend(mgen_validate::validate_graph(&g));
                Some(g)
            }
            Err(e) => {
                diags.push(mgen_core::Diagnostic::error(
                    "E0701",
                    format!("lowering error: {e}"),
                ));
                None
            }
        }
    };

    if json {
        print!("{}", mgen_validate::render_json(&filename, &diags));
    } else {
        mgen_validate::render_human(&filename, &src, &diags);
    }
    if mgen_core::has_errors(&diags) {
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

fn dump_scene(input: PathBuf, as_json: bool) -> Result<()> {
    let src = fs::read_to_string(&input)?;
    let ast = mgen_dsl::parse(&src)?;
    let scene = mgen_dsl::lower(&ast)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&scene)?);
    } else {
        println!("{:#?}", scene);
    }
    Ok(())
}

fn inspect(input: PathBuf) -> Result<()> {
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

fn print_gltf_summary(v: &serde_json::Value) {
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

struct GenerateArgs {
    prompt: String,
    out: Option<PathBuf>,
    dsl_out: Option<PathBuf>,
    seed: Option<u64>,
    model: String,
    dry_run: bool,
    budget_tokens: Option<u32>,
    max_repair_iters: u32,
    api_key: Option<String>,
    cached_content: Option<String>,
    no_cache: bool,
    temperature: Option<f32>,
    thinking: ThinkingLevel,
}

fn resolve_api_key(flag: Option<String>) -> Result<String> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(k);
    }
    let from_env = std::env::var("GEMINI_API_KEY").ok();
    match from_env {
        Some(k) if !k.trim().is_empty() => Ok(k),
        _ => bail!("missing GEMINI_API_KEY (set env var or pass --api-key)"),
    }
}

/// Create the parent directory for `path` if it doesn't already exist. Called
/// before any expensive work (like an LLM round-trip) so path errors surface up
/// front instead of after tokens are spent.
fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    Ok(())
}

/// Set `cfg.cached_content` or `cfg.system_instruction` based on flags.
///
/// Precedence:
///   1. `--cached-content <name>` — use that resource name verbatim (no local cache read/write).
///   2. `--no-cache` — send the system instruction inline.
///   3. Default — try to resolve or create a persistent cache entry under
///      `$MGEN_CACHE_DIR` / `$HOME/.cache/mgen/`, falling back to inline on
///      any failure (printed to stderr so the user notices repeat failures).
fn attach_system_instruction(
    cfg: &mut GenerateConfig,
    client: &GeminiClient,
    pinned: Option<String>,
    no_cache: bool,
    label: &str,
) {
    let system_text = system_instruction(&StdlibIndex::default());

    if let Some(name) = pinned {
        cfg.cached_content = Some(name);
        return;
    }
    if no_cache {
        cfg.system_instruction = Some(system_text);
        return;
    }

    let Some(cache_path) = default_cache_path() else {
        cfg.system_instruction = Some(system_text);
        return;
    };

    match resolve_or_create_cache(
        client,
        &cfg.model,
        &system_text,
        &cache_path,
        DEFAULT_TTL_SECONDS,
    ) {
        Ok(name) => {
            cfg.cached_content = Some(name);
        }
        Err(e) => {
            eprintln!(
                "mgen {label}: cache unavailable ({e}); sending system instruction inline"
            );
            cfg.system_instruction = Some(system_text);
        }
    }
}

/// Render the cached-token portion of a Gemini usage record. Returns an empty
/// string when the call didn't hit a cache, so the summary doesn't grow a
/// noisy "cached=0" suffix for inline runs.
fn format_cached_tokens(usage: &mgen_llm::Usage) -> String {
    if usage.cached_tokens > 0 {
        format!(", cached={}", usage.cached_tokens)
    } else {
        String::new()
    }
}

/// Deterministic-ish seed from the current time. Stable seeds come from --seed.
fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}

fn generate(args: GenerateArgs) -> Result<()> {
    if !args.dry_run && args.out.is_none() {
        bail!("--out is required unless --dry-run is set");
    }

    // Resolve output paths up front so we can create their parent directories
    // before burning tokens on the LLM call.
    let resolved_out = args.out.clone();
    let resolved_dsl_out: Option<PathBuf> = if args.dry_run {
        args.dsl_out.clone()
    } else {
        Some(
            args.dsl_out
                .clone()
                .unwrap_or_else(|| resolved_out.as_ref().unwrap().with_extension("mg")),
        )
    };
    if let Some(p) = resolved_out.as_deref() {
        ensure_parent_dir(p)?;
    }
    if let Some(p) = resolved_dsl_out.as_deref() {
        ensure_parent_dir(p)?;
    }

    let api_key = resolve_api_key(args.api_key)?;
    let client = GeminiClient::new(api_key);

    let seed = args.seed.unwrap_or_else(pick_default_seed);

    let mut cfg = GenerateConfig::new(&args.prompt);
    cfg.model = args.model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(args.thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "generate");

    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("generate: calling Gemini (attempt 1/{total_attempts})"),
        GEMINI_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "generate: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let outcome = match generate_with_repair(&client, cfg, &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("generate: Gemini error — {e}"));
            return Err(anyhow!("gemini: {e}"));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &args.prompt);

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "generate: DSL still invalid after {} call{} ({} tokens)",
            outcome.call_count,
            if outcome.call_count == 1 { "" } else { "s" },
            outcome.usage.total_tokens
        ));
        // Print diagnostics so the user knows what the LLM missed.
        let filename = "<generated>".to_string();
        if args.dry_run {
            eprintln!("{}", mgen_validate::render_json(&filename, &outcome.diagnostics));
            println!("{}", wrapped);
        } else {
            mgen_validate::render_human(&filename, &wrapped, &outcome.diagnostics);
        }
        bail!("refusing to build: validation errors in generated DSL");
    }

    pb.finish_with_message(format!(
        "generate: DSL ready — {} call{}, {} tokens (prompt={}, response={}{})",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
        outcome.usage.prompt_tokens,
        outcome.usage.response_tokens,
        format_cached_tokens(&outcome.usage),
    ));

    if args.dry_run {
        println!("{}", wrapped);
        if let Some(dsl_path) = resolved_dsl_out {
            fs::write(&dsl_path, &wrapped)
                .with_context(|| format!("writing {}", dsl_path.display()))?;
        }
        return Ok(());
    }

    let out_path = resolved_out.expect("out checked above");
    let dsl_path = resolved_dsl_out.expect("dsl_out resolved above for non-dry-run");

    fs::write(&dsl_path, &wrapped)
        .with_context(|| format!("writing {}", dsl_path.display()))?;

    // Reuse the regular build path so the user gets the same progress line.
    build(dsl_path, out_path)
}

struct ModifyArgs {
    input: PathBuf,
    prompt: String,
    out: Option<PathBuf>,
    dsl_out: Option<PathBuf>,
    seed: Option<u64>,
    model: String,
    dry_run: bool,
    budget_tokens: Option<u32>,
    max_repair_iters: u32,
    api_key: Option<String>,
    cached_content: Option<String>,
    no_cache: bool,
    temperature: Option<f32>,
    thinking: ThinkingLevel,
}

fn modify(args: ModifyArgs) -> Result<()> {
    let existing = fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;

    let seed = args
        .seed
        .or_else(|| parse_seed_header(&existing))
        .unwrap_or_else(pick_default_seed);

    // Resolve output paths up front so we can create their parent directories
    // before burning tokens on the LLM call.
    let resolved_dsl_out = args.dsl_out.clone().unwrap_or_else(|| args.input.clone());
    let resolved_out = args
        .out
        .clone()
        .unwrap_or_else(|| args.input.with_extension("glb"));
    if !args.dry_run {
        ensure_parent_dir(&resolved_dsl_out)?;
        ensure_parent_dir(&resolved_out)?;
    }

    let api_key = resolve_api_key(args.api_key)?;
    let client = GeminiClient::new(api_key);

    let user_prompt = format!(
        "You are editing an existing mgen DSL file. Apply this modification:\n\n\
    {mod_prompt}\n\n\
Make the smallest edit that satisfies the request. Do not rename, reorder, \
reformat, or restyle parts the modification does not touch — preserve their \
names, materials, transforms, connectors, attaches, joints, clips, and \
tracks verbatim. Do not \"improve\" unrelated geometry.\n\n\
When the edit adds a new primitive, it still needs a `material` (declare one \
or reuse an existing name) AND either an `attach` joining it to the rest of \
the scene or `tags=\"floating\"` on itself or an ancestor — otherwise the \
geometric connectivity validator (E1101) will reject it. When the edit \
removes or renames a node, update every reference to that name: `attach \
parent=`/`child=`, `joint pivot=`, animation `target=`, and any `socket`/\
`plug` that pointed at a removed connector.\n\n\
Reply with ONLY the full modified DSL — no commentary, no markdown fences, \
no diff markers. Emit the entire file, not just the changed region. Do not \
include the `// mgen-generate` header comments; the caller re-adds them.\n\n\
Existing file:\n\n{existing}",
        existing = existing.trim_end(),
        mod_prompt = args.prompt.trim(),
    );

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = args.model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(args.thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "modify");

    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("modify: calling Gemini (attempt 1/{total_attempts})"),
        GEMINI_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "modify: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let outcome = match generate_with_repair(&client, cfg, &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("modify: Gemini error — {e}"));
            return Err(anyhow!("gemini: {e}"));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &args.prompt);

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "modify: DSL still invalid after {} call{} ({} tokens)",
            outcome.call_count,
            if outcome.call_count == 1 { "" } else { "s" },
            outcome.usage.total_tokens
        ));
        let filename = args.input.to_string_lossy().to_string();
        if args.dry_run {
            eprintln!(
                "{}",
                mgen_validate::render_json(&filename, &outcome.diagnostics)
            );
            println!("{}", wrapped);
        } else {
            mgen_validate::render_human(&filename, &wrapped, &outcome.diagnostics);
        }
        bail!("refusing to build: validation errors in modified DSL");
    }

    pb.finish_with_message(format!(
        "modify: DSL ready — {} call{}, {} tokens (prompt={}, response={}{})",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
        outcome.usage.prompt_tokens,
        outcome.usage.response_tokens,
        format_cached_tokens(&outcome.usage),
    ));

    if args.dry_run {
        println!("{}", wrapped);
        return Ok(());
    }

    let dsl_path = resolved_dsl_out;
    let out_path = resolved_out;

    fs::write(&dsl_path, &wrapped)
        .with_context(|| format!("writing {}", dsl_path.display()))?;

    build(dsl_path, out_path)
}

fn bench(
    prompts_path: PathBuf,
    model: String,
    max_repair_iters: u32,
    budget_tokens: Option<u32>,
    api_key: Option<String>,
    no_cache: bool,
    thinking: ThinkingLevel,
) -> Result<()> {
    let api_key = resolve_api_key(api_key)?;
    let client = GeminiClient::new(api_key);

    let content = fs::read_to_string(&prompts_path)
        .with_context(|| format!("reading {}", prompts_path.display()))?;
    let prompts: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    if prompts.is_empty() {
        bail!("no prompts in {}", prompts_path.display());
    }

    // Resolve the cache once up front — every prompt in the batch shares the
    // same system instruction, so a single cache entry serves the whole run.
    let system = system_instruction(&StdlibIndex::default());
    let cached_name: Option<String> = if no_cache {
        None
    } else if let Some(cache_path) = default_cache_path() {
        match resolve_or_create_cache(&client, &model, &system, &cache_path, DEFAULT_TTL_SECONDS) {
            Ok(name) => Some(name),
            Err(e) => {
                eprintln!(
                    "mgen bench: cache unavailable ({e}); sending system instruction inline"
                );
                None
            }
        }
    } else {
        None
    };

    let mut successes = 0usize;
    let mut total_tokens = 0u64;
    let mut total_calls = 0u32;

    println!(
        "# mgen bench — {} prompts, model={}, max_repair_iters={}, cache={}",
        prompts.len(),
        model,
        max_repair_iters,
        if cached_name.is_some() { "on" } else { "off" },
    );

    for (i, prompt) in prompts.iter().enumerate() {
        let mut cfg = GenerateConfig::new(*prompt);
        cfg.model = model.clone();
        if let Some(name) = &cached_name {
            cfg.cached_content = Some(name.clone());
        } else {
            cfg.system_instruction = Some(system.clone());
        }
        cfg.budget_tokens = budget_tokens;
        cfg.thinking_level = Some(thinking);
        // Derive a deterministic seed per-prompt so reruns are comparable.
        cfg.seed = Some((i as u64).wrapping_add(1) * 0x9E37_79B1);

        let repair = RepairConfig { max_iters: max_repair_iters, on_iteration: None };

        match generate_with_repair(&client, cfg, &repair) {
            Ok(outcome) => {
                total_tokens += outcome.usage.total_tokens as u64;
                total_calls += outcome.call_count;
                if outcome.is_ok() {
                    successes += 1;
                    println!(
                        "[{:02}/{:02}] OK   calls={} tokens={} — {}",
                        i + 1,
                        prompts.len(),
                        outcome.call_count,
                        outcome.usage.total_tokens,
                        prompt
                    );
                } else {
                    let err_count = outcome
                        .diagnostics
                        .iter()
                        .filter(|d| matches!(d.severity, mgen_core::Severity::Error))
                        .count();
                    println!(
                        "[{:02}/{:02}] FAIL calls={} tokens={} errors={} — {}",
                        i + 1,
                        prompts.len(),
                        outcome.call_count,
                        outcome.usage.total_tokens,
                        err_count,
                        prompt
                    );
                }
            }
            Err(e) => {
                println!("[{:02}/{:02}] ERR  {} — {}", i + 1, prompts.len(), e, prompt);
            }
        }
    }

    let n = prompts.len();
    let success_rate = (successes as f32) * 100.0 / n as f32;
    let mean_tokens = if n > 0 { total_tokens as f64 / n as f64 } else { 0.0 };
    println!();
    println!(
        "# summary: {}/{} succeeded ({:.1}%), mean {:.0} tokens/prompt, {} total calls",
        successes, n, success_rate, mean_tokens, total_calls
    );
    if success_rate < 80.0 {
        bail!("bench target not met: {:.1}% < 80% success", success_rate);
    }
    Ok(())
}
