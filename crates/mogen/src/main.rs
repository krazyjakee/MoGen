mod commands;
mod common;
mod format;
mod spinner;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use mogen_llm::{Provider, ThinkingLevel};

use commands::animate::{animate, AnimateArgs};
use commands::auth::{dispatch as auth_dispatch, AuthCmd, AuthTarget, LoginCmd};
use commands::bench::bench;
use commands::build::build;
use commands::generate::{generate, GenerateArgs};
use commands::inspect::{check, dump_scene, inspect, parse_cmd};
use commands::modify::{modify, ModifyArgs};
use commands::moghub::{
    self as moghub_cmd, DiscoverArgs as MoghubDiscoverArgs, DownloadArgs as MoghubDownloadArgs,
    PublishArgs as MoghubPublishArgs,
};
use commands::repair::{repair, RepairArgs};
use commands::textures::textures_cmd;
use commands::thumbnail::{thumbnail, ThumbnailArgs};
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
    /// Fireworks AI's OpenAI-compatible Chat Completions surface. Default
    /// model is the Fire Pass `kimi-k2p6` router; set `FIREWORKS_API_KEY`.
    Fireworks,
    /// Z.ai (Zhipu) GLM family via the OpenAI-compatible chat endpoint.
    /// Default model is `glm-5.1`; set `ZAI_API_KEY`.
    Zai,
}

impl From<ProviderArg> for Provider {
    fn from(p: ProviderArg) -> Self {
        match p {
            ProviderArg::Gemini => Provider::Gemini,
            ProviderArg::Openai => Provider::OpenAI,
            ProviderArg::Anthropic => Provider::Anthropic,
            ProviderArg::Ollama => Provider::Ollama,
            ProviderArg::ClaudeCode => Provider::ClaudeCode,
            ProviderArg::Fireworks => Provider::Fireworks,
            ProviderArg::Zai => Provider::Zai,
        }
    }
}

/// CLI-facing mirror of [`commands::build::BuildFormat`]. Kept here so
/// `clap::ValueEnum` doesn't leak into the command module.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum BuildFormatArg {
    /// Binary glTF 2.0 (default).
    Glb,
    /// Autodesk FBX 7.4 binary.
    Fbx,
}

impl From<BuildFormatArg> for commands::build::BuildFormat {
    fn from(f: BuildFormatArg) -> Self {
        match f {
            BuildFormatArg::Glb => commands::build::BuildFormat::Glb,
            BuildFormatArg::Fbx => commands::build::BuildFormat::Fbx,
        }
    }
}

#[derive(Parser)]
#[command(name = "mogen", version, about = "Procedural 3D model generator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// `mogen auth <target> <verb>` — one subcommand per credential
/// `mogen` knows how to persist. Each target keeps its own login flag
/// set (`--no-browser`/`--timeout` for Google's loopback, `--server`
/// for MoGHub) so the help text never advertises a flag that does
/// nothing on the target it's listed under.
#[derive(Subcommand)]
enum AuthArg {
    /// Print a one-line login status for every target at a glance.
    /// Exits 0 if any target is logged in, 1 otherwise. With
    /// `--verbose`, also dumps the per-target detail block underneath.
    Status {
        #[arg(long)]
        verbose: bool,
    },
    /// Manage Google's gemini-cli OAuth bundle (text generation via
    /// Cloud Code Assist `v1internal:generateContent`). The bundled
    /// `client_id` / `client_secret` are Google's public Gemini CLI
    /// values, so login is zero-config.
    GeminiCli {
        #[command(subcommand)]
        cmd: OauthVerb,
    },
    /// Manage Google's Antigravity OAuth bundle (image generation via
    /// the Cloud Code Assist `:streamGenerateContent` surface, which
    /// rejects the gemini-cli client). Required for OAuth-driven
    /// `mogen textures`.
    Antigravity {
        #[command(subcommand)]
        cmd: OauthVerb,
    },
    /// Manage the MoGHub session token (community publishing /
    /// authenticated browsing). Loopback browser flow against
    /// `<server>/api/auth/desktop/start`. The same `~/.mogen/
    /// moghub_auth.json` file is read by Studio, so logging in once
    /// covers both surfaces.
    Moghub {
        #[command(subcommand)]
        cmd: MoghubVerb,
    },
}

/// Verbs available for both Google OAuth targets. `--no-browser` and
/// `--timeout` only make sense for the loopback flow and are absent on
/// `MoghubVerb`.
#[derive(Subcommand)]
enum OauthVerb {
    /// Open Google sign-in in the browser and store the resulting
    /// token bundle. Idempotent — already-logged-in is a no-op
    /// without `--force`.
    Login {
        /// Re-authenticate even if a valid token is already stored.
        #[arg(long)]
        force: bool,
        /// Don't open the system browser. Print the authorize URL
        /// instead so you can open it on another machine (useful over
        /// SSH).
        #[arg(long)]
        no_browser: bool,
        /// How long to wait (seconds) for the OAuth callback before
        /// giving up. Clamped to [10, 3600].
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Print the email + project the bundle resolves to, plus the
    /// access-token's remaining lifetime. Exits 0 when logged in
    /// (even if the access token is expired — refresh happens on next
    /// call), 1 when not logged in.
    Status {
        /// Also print the on-disk token-store path, OAuth scopes, the
        /// chosen `cloudcode-pa` endpoint, and a note when
        /// `GEMINI_API_KEY` shadows the OAuth credentials.
        #[arg(long)]
        verbose: bool,
    },
    /// Delete the stored token bundle. Idempotent — does not call the
    /// Google revoke endpoint, so the refresh token remains valid
    /// server-side until the user revokes consent at
    /// <https://myaccount.google.com>.
    Logout,
}

/// Verbs for the MoGHub session target. `--server` lets the user
/// authenticate against a self-hosted instance; the URL round-trips
/// into the on-disk token so future `status` calls reach the same
/// host.
#[derive(Subcommand)]
enum MoghubVerb {
    Login {
        /// Re-authenticate even if a session is already on disk.
        #[arg(long)]
        force: bool,
        /// MoGHub instance to sign in against. Defaults to the
        /// production server.
        #[arg(long)]
        server: Option<String>,
    },
    Status {
        /// Also call `whoami` against the server to confirm the
        /// stored token is still accepted.
        #[arg(long)]
        verbose: bool,
    },
    Logout,
}

/// Subcommands under `mogen moghub`. Read-only verbs work without a
/// session; write verbs (`publish`, `comment`, `like`/`unlike`,
/// `notifications`) require `mogen auth moghub login` to have stored
/// a session token.
#[derive(Subcommand)]
enum MoghubCmd {
    /// Print the signed-in user's handle and id. Exits non-zero if no
    /// session is active.
    Whoami {
        #[arg(long)]
        server: Option<String>,
    },
    /// Browse the public discover feed.
    Discover {
        /// Free-text search.
        #[arg(short, long)]
        query: Option<String>,
        /// Filter by kind: `scene`, `model`, `module`, or `all`.
        #[arg(long)]
        kind: Option<String>,
        /// Filter by tag.
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        /// Emit the raw API response as JSON.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        server: Option<String>,
    },
    /// Print full detail for a model. Reference is `<user>/<slug>`
    /// (or `@<user>/<slug>`).
    Info {
        reference: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        server: Option<String>,
    },
    /// Download a model's `.mog` files into a directory. Defaults to
    /// the latest version unless `--version` is given.
    Download {
        reference: String,
        #[arg(long)]
        version: Option<i32>,
        /// Destination directory. Defaults to `<slug>-v<version>` in
        /// the working directory.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Only fetch the entry `.mog`; skip imports and the
        /// thumbnail.
        #[arg(long)]
        entry_only: bool,
        #[arg(long)]
        server: Option<String>,
    },
    /// List comments on a model.
    Comments {
        reference: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// Post a comment. Body accepts MoGHub bbcode.
    Comment {
        reference: String,
        body: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// Like a model. Idempotent.
    Like {
        reference: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// Remove a previously-set like.
    Unlike {
        reference: String,
        #[arg(long)]
        server: Option<String>,
    },
    /// List the signed-in user's notifications.
    Notifications {
        /// Mark every notification as read instead of just listing.
        #[arg(long)]
        mark_read: bool,
        #[arg(long)]
        server: Option<String>,
    },
    /// Publish a `.mog` file to MoGHub. Bundles every locally
    /// imported `.mog` plus referenced PNG/JPG/JPEG/WebP textures.
    /// Re-publishing a file that already carries a
    /// `meta(moghub_model_id, moghub_slug, moghub_version)` stamp
    /// appends a new version to that model unless `--new` is given.
    Publish {
        input: PathBuf,
        /// Override `meta(name=…)` for this publish.
        #[arg(long)]
        title: Option<String>,
        /// Override `meta(description=…)`.
        #[arg(long)]
        description: Option<String>,
        /// Comma-separated tags. Overrides `meta(tags=[…])`.
        #[arg(long)]
        tags: Option<String>,
        /// SPDX-style license id. Defaults to `CC0-1.0`.
        #[arg(long)]
        license: Option<String>,
        /// `public`, `unlisted`, or `private`. Defaults to `public`.
        #[arg(long)]
        visibility: Option<String>,
        /// Version changelog message.
        #[arg(short, long)]
        message: Option<String>,
        /// Path to a PNG to attach as the model thumbnail. Optional —
        /// when omitted, the CLI renders one headlessly via `mogen-render`
        /// using the same orbit framing as the Studio's preview.
        #[arg(long)]
        thumbnail: Option<PathBuf>,
        /// Override the published filename. Defaults to the input
        /// file's basename.
        #[arg(long)]
        filename: Option<String>,
        /// Publish as a registry-importable module.
        #[arg(long, conflicts_with = "scene")]
        module: bool,
        /// Publish as a scene (default when the file has imports).
        #[arg(long, conflicts_with = "module")]
        scene: bool,
        /// Force creation of a new model even if `meta()` carries a
        /// prior MoGHub stamp.
        #[arg(long)]
        new: bool,
        #[arg(long)]
        server: Option<String>,
    },
}

impl From<AuthArg> for AuthCmd {
    fn from(a: AuthArg) -> Self {
        match a {
            AuthArg::Status { verbose } => AuthCmd::Status { target: None, verbose },
            AuthArg::GeminiCli { cmd } => convert_oauth(AuthTarget::GeminiCli, cmd),
            AuthArg::Antigravity { cmd } => convert_oauth(AuthTarget::Antigravity, cmd),
            AuthArg::Moghub { cmd } => convert_moghub(cmd),
        }
    }
}

fn convert_oauth(target: AuthTarget, verb: OauthVerb) -> AuthCmd {
    match verb {
        OauthVerb::Login { force, no_browser, timeout } => {
            let inner = match target {
                AuthTarget::GeminiCli => LoginCmd::GeminiCli {
                    force,
                    no_browser,
                    timeout_secs: timeout,
                },
                AuthTarget::Antigravity => LoginCmd::Antigravity {
                    force,
                    no_browser,
                    timeout_secs: timeout,
                },
                AuthTarget::Moghub => unreachable!("OauthVerb only feeds OAuth targets"),
            };
            AuthCmd::Login(inner)
        }
        OauthVerb::Status { verbose } => AuthCmd::Status {
            target: Some(target),
            verbose,
        },
        OauthVerb::Logout => AuthCmd::Logout { target },
    }
}

fn convert_moghub(verb: MoghubVerb) -> AuthCmd {
    match verb {
        MoghubVerb::Login { force, server } => {
            AuthCmd::Login(LoginCmd::Moghub { force, server })
        }
        MoghubVerb::Status { verbose } => AuthCmd::Status {
            target: Some(AuthTarget::Moghub),
            verbose,
        },
        MoghubVerb::Logout => AuthCmd::Logout {
            target: AuthTarget::Moghub,
        },
    }
}

fn dispatch_moghub(cmd: MoghubCmd) -> anyhow::Result<()> {
    match cmd {
        MoghubCmd::Whoami { server } => moghub_cmd::whoami(server),
        MoghubCmd::Discover {
            query,
            kind,
            tag,
            limit,
            offset,
            json,
            server,
        } => moghub_cmd::discover(MoghubDiscoverArgs {
            query,
            kind,
            tag,
            limit,
            offset,
            json,
            server,
        }),
        MoghubCmd::Info { reference, json, server } => moghub_cmd::info(reference, json, server),
        MoghubCmd::Download {
            reference,
            version,
            out,
            entry_only,
            server,
        } => moghub_cmd::download(MoghubDownloadArgs {
            reference,
            version,
            out,
            entry_only,
            server,
        }),
        MoghubCmd::Comments { reference, server } => moghub_cmd::comments(reference, server),
        MoghubCmd::Comment { reference, body, server } => {
            moghub_cmd::comment(reference, body, server)
        }
        MoghubCmd::Like { reference, server } => moghub_cmd::like(reference, false, server),
        MoghubCmd::Unlike { reference, server } => moghub_cmd::like(reference, true, server),
        MoghubCmd::Notifications { mark_read, server } => {
            moghub_cmd::notifications(mark_read, server)
        }
        MoghubCmd::Publish {
            input,
            title,
            description,
            tags,
            license,
            visibility,
            message,
            thumbnail,
            filename,
            module,
            scene,
            new,
            server,
        } => {
            let publish_as_module = if module {
                Some(true)
            } else if scene {
                Some(false)
            } else {
                None
            };
            moghub_cmd::publish(MoghubPublishArgs {
                input,
                title,
                description,
                tags,
                license,
                visibility,
                message,
                thumbnail,
                filename,
                publish_as_module,
                publish_as_new: new,
                server,
            })
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
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
        /// `reasoning.effort`. Ignored by Ollama. Falls back to the file's
        /// `meta(thinking=…)` attribute (modify/animate/repair only) and
        /// then to `high`.
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
        /// Gemini image model name. When omitted, defaults to
        /// `gemini-3-pro-image-preview` if you're signed in via
        /// `mogen auth login` (paid Cloud Code Assist surface) and
        /// `gemini-2.5-flash-image` otherwise (public API key).
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
        Cmd::Auth { cmd } => auth_dispatch(cmd.into()),
        Cmd::Moghub { cmd } => dispatch_moghub(cmd),
        Cmd::Build { input, out, format } => {
            // Default extension follows the chosen format. When no
            // `--format` is given and `--out` is omitted, fall back to
            // `.glb` to preserve the historical default; users wanting
            // FBX must either pass `-o foo.fbx` or `--format fbx` (which
            // then triggers the `.fbx` default extension).
            let default_ext = match format {
                Some(BuildFormatArg::Fbx) => "fbx",
                _ => "glb",
            };
            let out = out.unwrap_or_else(|| input.with_extension(default_ext));
            build(input, out, format.map(Into::into))
        }
        Cmd::Parse { input } => parse_cmd(input),
        Cmd::Check { input, json } => check(input, json),
        Cmd::DumpScene { input, json } => dump_scene(input, json),
        Cmd::Inspect { input } => inspect(input),
        Cmd::Thumbnail {
            input,
            out,
            size,
            yaw,
            pitch,
            bg,
        } => thumbnail(ThumbnailArgs {
            input,
            out,
            size,
            yaw,
            pitch,
            bg,
        }),
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
            zai_api_key,
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
            zai_api_key,
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
