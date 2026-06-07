//! Top-level [`Cmd`] enum — every `mogen <subcommand>` lives here. Kept
//! out of `main.rs` because the variants carry every flag in the CLI
//! surface and would otherwise bury the actual entry point.

use std::path::PathBuf;

use clap::Subcommand;

use super::auth::AuthArg;
use super::moghub::MoghubCmd;
use super::value_args::{BuildFormatArg, ProviderArg, StyleArg, ThinkingArg};

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Sign in / out for every credential `mogen` persists under
    /// `~/.mogen/`. Targets:
    ///
    /// - `gemini-cli` — Google OAuth for text gen via Cloud Code Assist.
    /// - `antigravity` — Google OAuth for image gen via Cloud Code Assist.
    /// - `moghub` — community session for publishing / authenticated
    ///   browsing. Studio reads the same on-disk session file.
    ///
    /// Run `mogen auth status` for a one-line summary per target, or
    /// `mogen auth <target> {login,status,logout}` for per-target
    /// management.
    Auth {
        #[command(subcommand)]
        cmd: AuthArg,
    },
    /// Browse, download, like, comment on, and publish to MoGHub —
    /// the same surface MoGen Studio's Community window exposes,
    /// driven from the terminal. Reads the session token written by
    /// `mogen auth moghub login`.
    Moghub {
        #[command(subcommand)]
        cmd: MoghubCmd,
    },
    /// Compile a DSL file to a binary scene container.
    ///
    /// Output format defaults to GLB but is auto-detected from the output
    /// extension (`.fbx` selects FBX). Pass `--format` to override.
    Build {
        input: PathBuf,
        /// Output path. Defaults to `<input>.glb` alongside the DSL file
        /// (or `<input>.fbx` when `--format fbx` is set).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Force a specific output container, ignoring the extension hint.
        #[arg(long, value_enum)]
        format: Option<BuildFormatArg>,
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
    /// Render a PNG preview of a `.mog` via the headless GL pipeline.
    /// Suitable to feed back into `mogen moghub publish --thumbnail`.
    Thumbnail {
        input: PathBuf,
        /// Output PNG path. Defaults to `<input>.png` alongside the DSL file.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Square edge length in pixels. Defaults to 512 (matches Studio).
        #[arg(long, default_value_t = 512)]
        size: u32,
        /// Camera yaw in radians. Defaults to π/4 (Studio default).
        #[arg(long)]
        yaw: Option<f32>,
        /// Camera pitch in radians. Defaults to 0.5 rad (~28°, Studio default).
        #[arg(long)]
        pitch: Option<f32>,
        /// Background fill as 6-digit hex (e.g. `#2a2d33`). Defaults to
        /// Studio's slate grey.
        #[arg(long)]
        bg: Option<String>,
    },
    /// Generate a DSL file from a natural-language prompt via the configured
    /// LLM provider, then validate and compile it.
    Generate {
        /// Natural-language description of the asset, e.g. "a wooden stool".
        prompt: String,
        /// Output GLB path. Ignored in --dry-run mode.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where to stash the intermediate DSL. Defaults to sibling of `out`
        /// with a .mog extension; required with --dry-run if you want the DSL.
        #[arg(long)]
        dsl_out: Option<PathBuf>,
        /// Seed embedded in the DSL header for reproducibility. Randomized if omitted.
        #[arg(long)]
        seed: Option<u64>,
        /// LLM provider. For Gemini, credentials are resolved via `--auth`
        /// (default `auto`) and `mogen auth login` (gemini-cli or
        /// Antigravity OAuth) or `GEMINI_API_KEY`. Other cloud providers use
        /// their matching `*_API_KEY` env var (`OPENAI_API_KEY`,
        /// `ANTHROPIC_API_KEY`, `XIAOMI_API_KEY`, ...); Ollama is keyless by default.
        #[arg(long, value_enum, default_value_t = ProviderArg::Auto)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default
        /// (Gemini Pro / GPT / Claude Sonnet / MiMo Pro / llama3.1).
        #[arg(long)]
        model: Option<String>,
        /// Print the generated DSL but skip compilation and GLB output.
        #[arg(long)]
        dry_run: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override the provider's API key env var.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        /// **Gemini only** — silently ignored for other providers.
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `MOGEN_CACHE_DIR`).
        /// Cache is Gemini-only; other providers always send the system
        /// instruction inline.
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Provider default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. Maps to Gemini's `thinkingBudget`,
        /// Anthropic's `thinking.budget_tokens`, and OpenAI's
        /// `reasoning.effort`. Ignored by Ollama. Falls back to the file's
        /// `meta(thinking=…)` attribute (modify/animate/repair only) and
        /// then to `high`.
        #[arg(long, value_enum)]
        thinking: Option<ThinkingArg>,
        /// Visual-style hint. Prepends a "## Style" guidance block to the
        /// prompt and stamps `meta(style=…)` into the saved DSL so future
        /// `modify` / `animate` / `repair` runs inherit the look. Omit
        /// for no style guidance.
        #[arg(long, value_enum)]
        style: Option<StyleArg>,
        /// Run the Architect agent first: generate a Markdown plan from the
        /// prompt, then feed that plan into the Coder pass. Splits spatial
        /// reasoning out of the DSL emission step so the model is less
        /// likely to drown in primitive coordinates. Costs one extra LLM
        /// round-trip; the planning call uses the same model + thinking
        /// level as the Coder pass.
        #[arg(long)]
        plan: bool,
        /// Render the generated DSL with the headless renderer and feed the
        /// PNG back to a vision-capable LLM for self-critique, repeating
        /// `N` times. Each iteration runs through the full validate +
        /// repair loop, so the final file is still guaranteed to compile.
        /// `0` (the default) skips refinement entirely. Requires a
        /// vision-capable provider.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=10))]
        auto_refine: u32,
    },
    /// Modify an existing DSL file with a natural-language prompt via the
    /// configured LLM provider, then validate and recompile the GLB.
    Modify {
        /// Existing .mog file to modify.
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
        /// LLM provider. See `generate --provider`.
        #[arg(long, value_enum, default_value_t = ProviderArg::Auto)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default.
        #[arg(long)]
        model: Option<String>,
        /// Print the modified DSL but skip compilation and file writes.
        #[arg(long)]
        dry_run: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override the provider's API key env var.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        /// **Gemini only.**
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `--no-cache`
        /// on `generate` for details).
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Provider default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. See `generate --thinking`.
        #[arg(long, value_enum)]
        thinking: Option<ThinkingArg>,
        /// Visual-style hint. Falls back to the file's `meta(style=…)`
        /// when omitted, so styled files stay styled across edits.
        #[arg(long, value_enum)]
        style: Option<StyleArg>,
        /// Run the Architect agent first. See `generate --plan`.
        #[arg(long)]
        plan: bool,
        /// Visual auto-refinement iteration count. See
        /// `generate --auto-refine`.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=10))]
        auto_refine: u32,
        /// Force the model to re-emit the entire DSL instead of returning
        /// SEARCH/REPLACE edit blocks against the existing file. The
        /// edit-block mode is the default and falls back to a full rewrite
        /// transparently when blocks don't apply, so this flag is only
        /// useful when the requested change is broad enough that surgical
        /// edits would be more fragile than a clean restatement.
        #[arg(long)]
        rewrite: bool,
    },
    /// Add or edit animations on an existing DSL file via the configured LLM
    /// provider, then validate and recompile the GLB. The LLM is restricted
    /// to animation top-level declarations (`joint`, `clip`/`track`, and the
    /// procedural templates `spin`, `open_close`, `wave`, `flap`, `idle`).
    Animate {
        /// Existing .mog file whose animations should be edited.
        input: PathBuf,
        /// Natural-language description of the animation, e.g.
        /// "make the door swing open" or "spin the top wheel at 120 rpm".
        prompt: String,
        /// Output GLB path. Defaults to `<input>.glb`. Ignored with --dry-run.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where to write the updated DSL. Defaults to modifying `input` in place.
        #[arg(long)]
        dsl_out: Option<PathBuf>,
        /// Seed embedded in the DSL header. Defaults to the seed parsed from
        /// the input's header, or a random seed if absent.
        #[arg(long)]
        seed: Option<u64>,
        /// LLM provider. See `generate --provider`.
        #[arg(long, value_enum, default_value_t = ProviderArg::Auto)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default.
        #[arg(long)]
        model: Option<String>,
        /// Print the updated DSL but skip compilation and file writes.
        #[arg(long)]
        dry_run: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override the provider's API key env var.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        /// **Gemini only.**
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `--no-cache`
        /// on `generate` for details).
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Provider default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. See `generate --thinking`.
        #[arg(long, value_enum)]
        thinking: Option<ThinkingArg>,
        /// Visual-style hint. Falls back to the file's `meta(style=…)`
        /// when omitted.
        #[arg(long, value_enum)]
        style: Option<StyleArg>,
    },
    /// Repair validation errors in an existing .mog file via the configured
    /// LLM provider. Runs the validator first and passes each diagnostic
    /// (with source excerpt, caret, and fix hint) back to the model, then
    /// re-validates. Exits successfully as a no-op if the file already
    /// validates.
    Repair {
        /// Existing .mog file to repair.
        input: PathBuf,
        /// Output GLB path. Defaults to `<input>.glb`. Ignored with --dry-run.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where to write the repaired DSL. Defaults to modifying `input` in place.
        #[arg(long)]
        dsl_out: Option<PathBuf>,
        /// Seed embedded in the DSL header. Defaults to the seed parsed from
        /// the input's header, or a random seed if absent.
        #[arg(long)]
        seed: Option<u64>,
        /// LLM provider. See `generate --provider`.
        #[arg(long, value_enum, default_value_t = ProviderArg::Auto)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default.
        #[arg(long)]
        model: Option<String>,
        /// Print the repaired DSL but skip compilation and file writes.
        #[arg(long)]
        dry_run: bool,
        /// Stop after rewriting the .mog; don't run build.
        #[arg(long)]
        no_build: bool,
        /// Abort if total prompt+response token count exceeds this value.
        #[arg(long)]
        budget_tokens: Option<u32>,
        /// Max number of repair iterations after the first attempt.
        #[arg(long, default_value_t = 2)]
        max_repair_iters: u32,
        /// Override the provider's API key env var.
        #[arg(long)]
        api_key: Option<String>,
        /// `cachedContents/...` resource name to use for the system instruction.
        /// **Gemini only.**
        #[arg(long)]
        cached_content: Option<String>,
        /// Disable the automatic system-instruction cache (see `--no-cache`
        /// on `generate` for details).
        #[arg(long)]
        no_cache: bool,
        /// Sampling temperature. Provider default is used when omitted.
        #[arg(long)]
        temperature: Option<f32>,
        /// Cap on server-side reasoning. See `generate --thinking`.
        #[arg(long, value_enum)]
        thinking: Option<ThinkingArg>,
        /// Visual-style hint. Falls back to the file's `meta(style=…)`
        /// when omitted.
        #[arg(long, value_enum)]
        style: Option<StyleArg>,
    },
    /// Generate PBR textures for every material in a .mog file: an LLM-drawn
    /// albedo via Gemini 2.5 Flash Image, plus locally-derived normal,
    /// metallic-roughness, and occlusion maps (Sobel-from-luminance,
    /// variance-based, cavity-based). PNGs are written next to the .mog and
    /// the matching `*_texture="…"` attrs are spliced into each material.
    ///
    /// **This command is Gemini-only** — image generation isn't part of the
    /// abstraction, so OpenAI / Anthropic / Ollama are not selectable here.
    /// Per-slot, materials that already declare a given `*_texture` attr —
    /// or whose target PNG already exists at the planned path on disk — are
    /// skipped unless `--force` is passed. Existing on-disk PNGs still get
    /// their `*_texture` attr spliced into the source, just without an API
    /// call or local re-derivation.
    Textures {
        /// Input .mog file to augment.
        input: PathBuf,
        /// Where to write the modified .mog. Defaults to editing `input` in place.
        #[arg(long)]
        out: Option<PathBuf>,
        /// GLB output path. Defaults to `<input>.glb`.
        #[arg(long)]
        glb: Option<PathBuf>,
        /// Directory (relative to the .mog) where PNGs are written.
        /// Defaults to `textures/<mog-stem>/` so sibling assets don't collide
        /// on shared material names.
        #[arg(long)]
        textures_dir: Option<PathBuf>,
        /// Style hint appended to each image prompt.
        #[arg(long, default_value = "photorealistic")]
        style: String,
        /// Gemini image model name. When omitted, defaults to
        /// `gemini-3.1-flash-image` if you're signed in via
        /// `mogen auth antigravity login` (Cloud Code Assist image surface)
        /// and `gemini-2.5-flash-image` otherwise (public API key).
        #[arg(long)]
        model: Option<String>,
        /// Regenerate slots whose attr is already declared in the .mog or
        /// whose PNG already exists on disk at the planned path.
        #[arg(long)]
        force: bool,
        /// Print the plan and skip all API calls and file writes.
        #[arg(long)]
        dry_run: bool,
        /// Stop after rewriting the .mog; don't run build.
        #[arg(long)]
        no_build: bool,
        /// Override GEMINI_API_KEY.
        #[arg(long)]
        api_key: Option<String>,
        /// Use Z.ai's `glm-image` endpoint instead of Gemini for albedo
        /// generation. Pass the bearer key here, or set `ZAI_API_KEY` in
        /// your environment. Useful when Gemini is rate-limited.
        #[arg(long)]
        zai_api_key: Option<String>,
        /// Skip every derived PBR map (normal / metallic-roughness / AO).
        /// Albedo is still generated.
        #[arg(long)]
        no_pbr: bool,
        /// Skip the derived normal map.
        #[arg(long)]
        no_normal: bool,
        /// Skip the derived metallic-roughness map.
        #[arg(long)]
        no_metallic_roughness: bool,
        /// Skip the derived ambient-occlusion map.
        #[arg(long)]
        no_occlusion: bool,
        /// Cap (in pixels) on the longer side of every generated albedo.
        /// Derived PBR maps inherit this size, so this is the single lever
        /// for embedded-texture footprint. `0` keeps the model's native
        /// resolution (typically 1024²).
        #[arg(long, default_value_t = mogen_llm::textures::DEFAULT_TEXTURE_SIZE)]
        texture_size: u32,
    },
    /// Download the latest release from GitHub and replace the running
    /// `mogen` (and sibling `mogen-studio`) binary in place. By default this
    /// only checks for an update and prints what it would do — pass `--yes`
    /// to actually install. Updates are matched against the host's target
    /// triple from the assets uploaded by the project's release workflow.
    Update {
        /// Install the update without prompting.
        #[arg(long)]
        yes: bool,
        /// Print the latest release tag and exit without downloading anything.
        #[arg(long)]
        check: bool,
        /// Reinstall the latest release even if the running binary already
        /// matches it. Useful for repairing a corrupted install.
        #[arg(long)]
        force: bool,
    },
    /// Privileged half of the auto-updater. Hidden because no user types
    /// this by hand — `download_and_apply` re-launches it under pkexec /
    /// sudo / UAC when the install directory isn't writable. The plan file
    /// is JSON describing a list of `src -> dst` moves to perform.
    #[command(name = "__apply-update", hide = true)]
    ApplyUpdate {
        /// Path to the plan JSON written by the unprivileged caller.
        #[arg(long)]
        plan: PathBuf,
    },
    /// Run `mogen` as an MCP (Model Context Protocol) server over stdio.
    /// Every other CLI subcommand — `build`, `check`, `generate`,
    /// `moghub publish`, etc. — is exposed as a tool the connected LLM
    /// client can invoke. Each tool call spawns the same binary in
    /// normal CLI mode and captures stdout/stderr, so behaviour is
    /// identical to invoking the command from a terminal. Designed to
    /// be launched by an MCP client (e.g. Claude Desktop / a custom
    /// client) — it speaks JSON-RPC on stdin/stdout, so don't run it
    /// interactively.
    Mcp,
    /// Run a suite of prompts through `generate` and report success rate and
    /// mean token cost. Does not write GLBs.
    Bench {
        /// File with one prompt per line; `#` starts a comment. Defaults to
        /// `benches/prompts.txt` in the project root.
        #[arg(long, default_value = "benches/prompts.txt")]
        prompts: PathBuf,
        /// LLM provider. See `generate --provider`.
        #[arg(long, value_enum, default_value_t = ProviderArg::Auto)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default.
        #[arg(long)]
        model: Option<String>,
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
