mod commands;
mod common;
mod format;
mod spinner;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use mogen_llm::{Provider, ThinkingLevel, DEFAULT_IMAGE_MODEL};

use commands::animate::{animate, AnimateArgs};
use commands::bench::bench;
use commands::build::build;
use commands::generate::{generate, GenerateArgs};
use commands::inspect::{check, dump_scene, inspect, parse_cmd};
use commands::modify::{modify, ModifyArgs};
use commands::repair::{repair, RepairArgs};
use commands::textures::textures_cmd;
use commands::update::{update, UpdateArgs};

/// CLI-facing mirror of [`ThinkingLevel`]. Kept separate so we don't leak
/// `clap::ValueEnum` into the `mogen-llm` library crate.
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

/// CLI-facing mirror of [`Provider`]. Same separation as `ThinkingArg` —
/// keeps `clap::ValueEnum` out of the library crate.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProviderArg {
    Gemini,
    Openai,
    Anthropic,
    Ollama,
    /// Local `claude` CLI (Claude Code subscription). Auth is handled by
    /// the user's `claude /login`; no API key flag is required.
    ClaudeCode,
}

impl From<ProviderArg> for Provider {
    fn from(p: ProviderArg) -> Self {
        match p {
            ProviderArg::Gemini => Provider::Gemini,
            ProviderArg::Openai => Provider::OpenAI,
            ProviderArg::Anthropic => Provider::Anthropic,
            ProviderArg::Ollama => Provider::Ollama,
            ProviderArg::ClaudeCode => Provider::ClaudeCode,
        }
    }
}

#[derive(Parser)]
#[command(name = "mogen", version, about = "Procedural 3D model generator")]
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
        /// LLM provider. The matching API key env var is read for the call
        /// (`GEMINI_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` /
        /// `OLLAMA_API_KEY`). Ollama runs locally and is keyless by default.
        #[arg(long, value_enum, default_value_t = ProviderArg::Gemini)]
        provider: ProviderArg,
        /// Model name. When omitted, falls back to the provider's default
        /// (Gemini Pro / GPT-4o / Claude Sonnet / llama3.1).
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
        /// `reasoning.effort`. Ignored by Ollama. Falls back to the
        /// `// mogen-generate thinking=…` header (modify/animate/repair only)
        /// and then to `high`.
        #[arg(long, value_enum)]
        thinking: Option<ThinkingArg>,
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
        #[arg(long, value_enum, default_value_t = ProviderArg::Gemini)]
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
        #[arg(long, value_enum, default_value_t = ProviderArg::Gemini)]
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
        #[arg(long, value_enum, default_value_t = ProviderArg::Gemini)]
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
        /// Gemini image model name.
        #[arg(long, default_value = DEFAULT_IMAGE_MODEL)]
        model: String,
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
    /// Run a suite of prompts through `generate` and report success rate and
    /// mean token cost. Does not write GLBs.
    Bench {
        /// File with one prompt per line; `#` starts a comment. Defaults to
        /// `benches/prompts.txt` in the project root.
        #[arg(long, default_value = "benches/prompts.txt")]
        prompts: PathBuf,
        /// LLM provider. See `generate --provider`.
        #[arg(long, value_enum, default_value_t = ProviderArg::Gemini)]
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
            provider,
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
            provider: provider.into(),
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.map(Into::into),
        }),
        Cmd::Modify {
            input,
            prompt,
            out,
            dsl_out,
            seed,
            provider,
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
            provider: provider.into(),
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.map(Into::into),
        }),
        Cmd::Animate {
            input,
            prompt,
            out,
            dsl_out,
            seed,
            provider,
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking,
        } => animate(AnimateArgs {
            input,
            prompt,
            out,
            dsl_out,
            seed,
            provider: provider.into(),
            model,
            dry_run,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.map(Into::into),
        }),
        Cmd::Repair {
            input,
            out,
            dsl_out,
            seed,
            provider,
            model,
            dry_run,
            no_build,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking,
        } => repair(RepairArgs {
            input,
            out,
            dsl_out,
            seed,
            provider: provider.into(),
            model,
            dry_run,
            no_build,
            budget_tokens,
            max_repair_iters,
            api_key,
            cached_content,
            no_cache,
            temperature,
            thinking: thinking.map(Into::into),
        }),
        Cmd::Textures {
            input,
            out,
            glb,
            textures_dir,
            style,
            model,
            force,
            dry_run,
            no_build,
            api_key,
            no_pbr,
            no_normal,
            no_metallic_roughness,
            no_occlusion,
            texture_size,
        } => textures_cmd(mogen_llm::textures::TexturesArgs {
            textures_dir: textures_dir
                .unwrap_or_else(|| mogen_llm::textures::default_textures_dir(&input)),
            input,
            out,
            glb,
            style,
            model,
            force,
            dry_run,
            no_build,
            api_key,
            no_pbr,
            no_normal,
            no_metallic_roughness,
            no_occlusion,
            texture_size,
        }),
        Cmd::Update { yes, check, force } => update(UpdateArgs {
            yes,
            check_only: check,
            force,
        }),
        Cmd::Bench {
            prompts,
            provider,
            model,
            max_repair_iters,
            budget_tokens,
            api_key,
            no_cache,
            thinking,
        } => bench(
            prompts,
            provider.into(),
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
