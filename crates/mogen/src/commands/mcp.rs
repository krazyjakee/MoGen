//! `mogen mcp` — expose every CLI subcommand to an MCP-speaking LLM
//! client over stdio.
//!
//! Each tool wrapper builds an argv vector and re-invokes the same
//! `mogen` binary as a subprocess. Going via subprocess (rather than
//! calling the command functions in-process) buys us three things:
//!
//! * Several commands call `std::process::exit(1)` on validation errors
//!   (`check`, `auth`, `moghub`). Running them in-process would kill the
//!   MCP server itself.
//! * Many commands `println!` results to stdout. In stdio MCP mode our
//!   own stdout is reserved for JSON-RPC, so any stray print would
//!   corrupt the protocol. The child writes its stdout into a pipe we
//!   capture.
//! * Future CLI changes (new flags, new subcommands) need no MCP-side
//!   refactor — the args are forwarded verbatim.
//!
//! Tool names mirror the CLI subcommand names with hyphens replaced by
//! underscores (clap auto-renames `DumpScene` to `dump-scene` on the
//! CLI; we call the tool `dump_scene`). Moghub tools are namespaced
//! with a `moghub_` prefix.

use std::path::PathBuf;

use anyhow::Result;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use tokio::process::Command;
use tracing_subscriber::EnvFilter;

/// Entry point for `mogen mcp`. Builds a current-thread tokio runtime
/// (no need for a thread pool — every tool dispatches to a child
/// process), wires the MCP server to stdio, and blocks until the
/// client disconnects.
pub(crate) fn run() -> Result<()> {
    // Logs MUST go to stderr — stdout is the JSON-RPC transport. If
    // `RUST_LOG` is unset, default to `warn` so a noisy environment
    // can't accidentally pollute the protocol via tracing macros
    // someone else writes.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let service = MogenMcp::new().serve(stdio()).await?;
        service.waiting().await?;
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

// --- subprocess plumbing ----------------------------------------------------

/// Spawn the running `mogen` binary with `args`, capture stdout +
/// stderr, and return them as MCP tool content.
///
/// On non-zero exit we still return a successful `CallToolResult` but
/// mark it as an error result and bundle stderr in the message so the
/// calling LLM gets the diagnostics back instead of an opaque
/// "execution failed". On spawn failure (binary missing, OS error) we
/// return a hard `McpError`.
async fn run_mogen(args: Vec<String>) -> Result<CallToolResult, McpError> {
    let exe = std::env::current_exe()
        .map_err(|e| McpError::internal_error(format!("locating mogen binary: {e}"), None))?;
    let output = Command::new(&exe)
        .args(&args)
        .output()
        .await
        .map_err(|e| McpError::internal_error(format!("spawning mogen: {e}"), None))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut parts = Vec::with_capacity(2);
    if !stdout.is_empty() {
        parts.push(Content::text(stdout));
    }
    if !stderr.is_empty() {
        parts.push(Content::text(format!("[stderr]\n{stderr}")));
    }
    if parts.is_empty() {
        parts.push(Content::text(""));
    }
    if output.status.success() {
        Ok(CallToolResult::success(parts))
    } else {
        // Surface a structured error result. Per the MCP spec, tool
        // errors that should still be returned to the LLM (rather than
        // failing the RPC) go through `CallToolResult` with
        // `is_error=true`.
        Ok(CallToolResult::error(parts))
    }
}

/// Helpers for assembling CLI args. The clap surface mostly uses
/// `--kebab-case` long flags and stringly-typed enums — these match.
fn push_opt<T: ToString>(args: &mut Vec<String>, name: &str, value: Option<T>) {
    if let Some(v) = value {
        args.push(name.to_string());
        args.push(v.to_string());
    }
}

fn push_flag(args: &mut Vec<String>, name: &str, value: bool) {
    if value {
        args.push(name.to_string());
    }
}

fn push_path_opt(args: &mut Vec<String>, name: &str, value: Option<PathBuf>) {
    if let Some(v) = value {
        args.push(name.to_string());
        args.push(v.to_string_lossy().into_owned());
    }
}

// --- shared parameter structs -----------------------------------------------

/// Subset of generation/modify/animate/repair flags that map 1:1 onto
/// the CLI. Each is optional — only `prompt` (or `input`) is required
/// per tool. Mirrors the clap definitions in `cli/cmd.rs` but stripped
/// to the JSON-shaped subset (no path-vs-string distinction, no
/// `value_enum` enums — strings are validated by the child).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LlmCommon {
    /// LLM provider. One of `auto`, `gemini`, `gemini-oauth`,
    /// `antigravity`, `openai`, `anthropic`, `ollama`, `claude-code`,
    /// `fireworks`, `zai`, `xiaomi`. Defaults to `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// Model name. Provider default applies when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Cap on server-side reasoning: `low`, `medium`, `high`, `xhigh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    /// Visual style hint. One of `ps1`, `n64`, `low-poly`,
    /// `high-detail`, `arcade`, `voxel`, `cel-shaded`,
    /// `stylized-fantasy`, `cyberpunk`, `pixel-art`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    /// Override the provider's API key env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    /// Sampling temperature; provider default applies when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Abort if total prompt+response tokens exceed this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
    /// Cap on repair iterations after the first attempt. Defaults to 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_repair_iters: Option<u32>,
    /// Seed embedded in the DSL header. Random when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    /// `cachedContents/...` resource for the system instruction.
    /// Gemini only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cached_content: Option<String>,
    /// Disable the automatic system-instruction cache.
    #[serde(default)]
    no_cache: bool,
    /// Print the generated DSL but skip compilation and file writes.
    #[serde(default)]
    dry_run: bool,
}

impl LlmCommon {
    fn push(self, args: &mut Vec<String>) {
        push_opt(args, "--provider", self.provider);
        push_opt(args, "--model", self.model);
        push_opt(args, "--thinking", self.thinking);
        push_opt(args, "--style", self.style);
        push_opt(args, "--api-key", self.api_key);
        push_opt(args, "--temperature", self.temperature);
        push_opt(args, "--budget-tokens", self.budget_tokens);
        push_opt(args, "--max-repair-iters", self.max_repair_iters);
        push_opt(args, "--seed", self.seed);
        push_opt(args, "--cached-content", self.cached_content);
        push_flag(args, "--no-cache", self.no_cache);
        push_flag(args, "--dry-run", self.dry_run);
    }
}

// --- per-tool parameter structs ---------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BuildArgs {
    /// Path to the `.mog` source.
    input: PathBuf,
    /// Output path. Defaults to `<input>.glb` (or `.fbx` when
    /// `format` is `fbx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Output container: `glb` (default) or `fbx`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InputOnly {
    /// Path to the `.mog` (for parse/dump-scene/check) or `.glb`
    /// (for inspect) input.
    input: PathBuf,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CheckArgs {
    /// Path to the `.mog` source.
    input: PathBuf,
    /// Emit diagnostics as line-delimited JSON instead of human-formatted text.
    #[serde(default)]
    json: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DumpSceneArgs {
    /// Path to the `.mog` source.
    input: PathBuf,
    /// Emit the lowered scene graph as JSON.
    #[serde(default)]
    json: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ThumbnailArgs {
    /// Path to the `.mog` source.
    input: PathBuf,
    /// Output PNG path. Defaults to `<input>.png`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Square edge length in pixels. Defaults to 512.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u32>,
    /// Camera yaw in radians. Default π/4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    yaw: Option<f32>,
    /// Camera pitch in radians. Default 0.5 rad.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pitch: Option<f32>,
    /// Background fill, 6-digit hex (`#2a2d33`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bg: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GenerateArgs {
    /// Natural-language asset description, e.g. "a wooden stool".
    prompt: String,
    /// Output GLB path. Ignored in dry-run mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Where to stash the intermediate `.mog`. Defaults to the sibling
    /// of `out` with a `.mog` extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dsl_out: Option<PathBuf>,
    /// Run the Architect agent first — generate a Markdown plan from
    /// the prompt, then feed it into the Coder pass. Costs one extra
    /// LLM round-trip.
    #[serde(default)]
    plan: bool,
    /// Visual auto-refinement iteration count (0–10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_refine: Option<u32>,
    #[serde(flatten)]
    common: LlmCommon,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ModifyArgs {
    /// Existing `.mog` to modify.
    input: PathBuf,
    /// Natural-language description of the change.
    prompt: String,
    /// Output GLB path. Defaults to `<input>.glb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Where to write the modified DSL. Defaults to overwriting `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dsl_out: Option<PathBuf>,
    /// Run the Architect agent first.
    #[serde(default)]
    plan: bool,
    /// Visual auto-refinement iteration count (0–10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_refine: Option<u32>,
    /// Force a full DSL rewrite instead of SEARCH/REPLACE edit blocks.
    #[serde(default)]
    rewrite: bool,
    #[serde(flatten)]
    common: LlmCommon,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AnimateArgs {
    /// Existing `.mog` whose animations should be edited.
    input: PathBuf,
    /// Natural-language description of the animation.
    prompt: String,
    /// Output GLB path. Defaults to `<input>.glb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Where to write the updated DSL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dsl_out: Option<PathBuf>,
    #[serde(flatten)]
    common: LlmCommon,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RepairArgs {
    /// Existing `.mog` to repair.
    input: PathBuf,
    /// Output GLB path. Defaults to `<input>.glb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Where to write the repaired DSL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dsl_out: Option<PathBuf>,
    /// Stop after rewriting the `.mog`; don't run build.
    #[serde(default)]
    no_build: bool,
    #[serde(flatten)]
    common: LlmCommon,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TexturesArgs {
    /// Input `.mog` to augment.
    input: PathBuf,
    /// Where to write the modified `.mog`. Defaults to in-place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// GLB output path. Defaults to `<input>.glb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    glb: Option<PathBuf>,
    /// PNG output directory, relative to the `.mog`. Defaults to
    /// `textures/<mog-stem>/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    textures_dir: Option<PathBuf>,
    /// Style hint appended to each image prompt. Defaults to
    /// `photorealistic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    /// Gemini image model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// Regenerate even if the slot is already declared or its PNG
    /// already exists.
    #[serde(default)]
    force: bool,
    /// Print the plan and skip API calls / file writes.
    #[serde(default)]
    dry_run: bool,
    /// Stop after rewriting the `.mog`; don't run build.
    #[serde(default)]
    no_build: bool,
    /// Override `GEMINI_API_KEY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    /// Use Z.ai's `glm-image` endpoint for albedo generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zai_api_key: Option<String>,
    /// Skip every derived PBR map (normal / metallic-roughness / AO).
    #[serde(default)]
    no_pbr: bool,
    /// Skip the derived normal map.
    #[serde(default)]
    no_normal: bool,
    /// Skip the derived metallic-roughness map.
    #[serde(default)]
    no_metallic_roughness: bool,
    /// Skip the derived ambient-occlusion map.
    #[serde(default)]
    no_occlusion: bool,
    /// Cap on the longer side of every generated albedo. `0` keeps
    /// the model's native resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    texture_size: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateArgs {
    /// Install the update without prompting.
    #[serde(default)]
    yes: bool,
    /// Print the latest release tag and exit without downloading.
    #[serde(default)]
    check: bool,
    /// Reinstall the latest release even if already at it.
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BenchArgs {
    /// One-prompt-per-line file. Defaults to `benches/prompts.txt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompts: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_repair_iters: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(default)]
    no_cache: bool,
}

// --- moghub parameter structs -----------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ServerOnly {
    /// MoGHub server base URL. Defaults to the canonical instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubDiscoverArgs {
    /// Free-text search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    /// Filter by kind: `scene`, `model`, `module`, or `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    /// Filter by tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offset: Option<u32>,
    /// Emit the raw API response as JSON.
    #[serde(default)]
    json: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubRefArgs {
    /// Model reference: `<user>/<slug>` or `@<user>/<slug>`.
    reference: String,
    #[serde(default)]
    json: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubDownloadArgs {
    /// Model reference: `<user>/<slug>` or `@<user>/<slug>`.
    reference: String,
    /// Specific version. Defaults to latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<i32>,
    /// Destination directory. Defaults to `<slug>-v<version>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    out: Option<PathBuf>,
    /// Only fetch the entry `.mog`; skip imports and thumbnail.
    #[serde(default)]
    entry_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubRefOnly {
    reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubCommentArgs {
    reference: String,
    /// BBCode comment body.
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubNotificationsArgs {
    /// Mark every notification as read instead of just listing.
    #[serde(default)]
    mark_read: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoghubPublishArgs {
    input: PathBuf,
    /// Override `meta(name=…)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Override `meta(description=…)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Comma-separated tags. Overrides `meta(tags=[…])`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<String>,
    /// SPDX-style license id. Defaults to `CC0-1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    license: Option<String>,
    /// `public`, `unlisted`, or `private`. Defaults to `public`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visibility: Option<String>,
    /// Version changelog message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// PNG to attach as the thumbnail. Auto-rendered when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thumbnail: Option<PathBuf>,
    /// Override the published filename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    /// Publish as a registry-importable module.
    #[serde(default)]
    module: bool,
    /// Publish as a scene.
    #[serde(default)]
    scene: bool,
    /// Force a new model even if `meta()` carries a prior stamp.
    #[serde(default)]
    new: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<String>,
}

// --- the server -------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct MogenMcp {
    // Read by the `#[tool_handler]` macro's generated dispatch — the
    // compiler can't see the indirection so it flags this as dead.
    #[allow(dead_code)]
    tool_router: ToolRouter<MogenMcp>,
}

#[tool_router]
impl MogenMcp {
    pub(crate) fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    // -- pipeline ----------------------------------------------------------

    #[tool(
        description = "Compile a .mog DSL file to a binary scene container (GLB or FBX). \
        The output extension picks the format unless `format` is set explicitly."
    )]
    async fn build(
        &self,
        Parameters(p): Parameters<BuildArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["build".to_string(), p.input.to_string_lossy().into_owned()];
        push_path_opt(&mut args, "--out", p.out);
        push_opt(&mut args, "--format", p.format);
        run_mogen(args).await
    }

    #[tool(description = "Parse a .mog file and print the AST.")]
    async fn parse(
        &self,
        Parameters(p): Parameters<InputOnly>,
    ) -> Result<CallToolResult, McpError> {
        run_mogen(vec![
            "parse".to_string(),
            p.input.to_string_lossy().into_owned(),
        ])
        .await
    }

    #[tool(
        description = "Validate a .mog file (semantic + reference checks). Returns \
        diagnostics; tool result is marked as error iff any diagnostic is a hard error."
    )]
    async fn check(
        &self,
        Parameters(p): Parameters<CheckArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["check".to_string(), p.input.to_string_lossy().into_owned()];
        push_flag(&mut args, "--json", p.json);
        run_mogen(args).await
    }

    #[tool(description = "Lower a .mog file to a SceneGraph and dump it as JSON or pretty Debug.")]
    async fn dump_scene(
        &self,
        Parameters(p): Parameters<DumpSceneArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "dump-scene".to_string(),
            p.input.to_string_lossy().into_owned(),
        ];
        push_flag(&mut args, "--json", p.json);
        run_mogen(args).await
    }

    #[tool(
        description = "Read a .glb file and print its structure (chunks, accessors, meshes, \
        materials, animations, skins)."
    )]
    async fn inspect(
        &self,
        Parameters(p): Parameters<InputOnly>,
    ) -> Result<CallToolResult, McpError> {
        run_mogen(vec![
            "inspect".to_string(),
            p.input.to_string_lossy().into_owned(),
        ])
        .await
    }

    #[tool(
        description = "Render a PNG preview of a .mog via the headless GL pipeline. Suitable \
        to feed back into moghub_publish as a thumbnail."
    )]
    async fn thumbnail(
        &self,
        Parameters(p): Parameters<ThumbnailArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "thumbnail".to_string(),
            p.input.to_string_lossy().into_owned(),
        ];
        push_path_opt(&mut args, "--out", p.out);
        push_opt(&mut args, "--size", p.size);
        push_opt(&mut args, "--yaw", p.yaw);
        push_opt(&mut args, "--pitch", p.pitch);
        push_opt(&mut args, "--bg", p.bg);
        run_mogen(args).await
    }

    // -- LLM-driven ---------------------------------------------------------

    #[tool(
        description = "Generate a .mog from a natural-language prompt via the configured LLM \
        provider, validate, and compile to GLB. Uses mogen's own LLM credentials."
    )]
    async fn generate(
        &self,
        Parameters(p): Parameters<GenerateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["generate".to_string(), p.prompt];
        push_path_opt(&mut args, "--out", p.out);
        push_path_opt(&mut args, "--dsl-out", p.dsl_out);
        push_flag(&mut args, "--plan", p.plan);
        push_opt(&mut args, "--auto-refine", p.auto_refine);
        p.common.push(&mut args);
        run_mogen(args).await
    }

    #[tool(
        description = "Modify an existing .mog with a natural-language prompt, validate, and \
        recompile. Default mode emits SEARCH/REPLACE edit blocks; set `rewrite=true` to force a \
        full rewrite."
    )]
    async fn modify(
        &self,
        Parameters(p): Parameters<ModifyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "modify".to_string(),
            p.input.to_string_lossy().into_owned(),
            p.prompt,
        ];
        push_path_opt(&mut args, "--out", p.out);
        push_path_opt(&mut args, "--dsl-out", p.dsl_out);
        push_flag(&mut args, "--plan", p.plan);
        push_opt(&mut args, "--auto-refine", p.auto_refine);
        push_flag(&mut args, "--rewrite", p.rewrite);
        p.common.push(&mut args);
        run_mogen(args).await
    }

    #[tool(
        description = "Add or edit animations on an existing .mog via the LLM. Restricted to \
        animation declarations (joint, clip/track, spin, open_close, wave, flap, idle)."
    )]
    async fn animate(
        &self,
        Parameters(p): Parameters<AnimateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "animate".to_string(),
            p.input.to_string_lossy().into_owned(),
            p.prompt,
        ];
        push_path_opt(&mut args, "--out", p.out);
        push_path_opt(&mut args, "--dsl-out", p.dsl_out);
        p.common.push(&mut args);
        run_mogen(args).await
    }

    #[tool(
        description = "Repair validation errors in an existing .mog via the LLM. Runs the \
        validator first, feeds each diagnostic back to the model, and re-validates. No-op if the \
        file already validates."
    )]
    async fn repair(
        &self,
        Parameters(p): Parameters<RepairArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["repair".to_string(), p.input.to_string_lossy().into_owned()];
        push_path_opt(&mut args, "--out", p.out);
        push_path_opt(&mut args, "--dsl-out", p.dsl_out);
        push_flag(&mut args, "--no-build", p.no_build);
        p.common.push(&mut args);
        run_mogen(args).await
    }

    #[tool(
        description = "Generate PBR textures for every material in a .mog (LLM-drawn albedo \
        + locally-derived normal / metallic-roughness / occlusion). Gemini-only. PNGs are \
        written next to the .mog and the matching texture attrs spliced into the source."
    )]
    async fn textures(
        &self,
        Parameters(p): Parameters<TexturesArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "textures".to_string(),
            p.input.to_string_lossy().into_owned(),
        ];
        push_path_opt(&mut args, "--out", p.out);
        push_path_opt(&mut args, "--glb", p.glb);
        push_path_opt(&mut args, "--textures-dir", p.textures_dir);
        push_opt(&mut args, "--style", p.style);
        push_opt(&mut args, "--model", p.model);
        push_flag(&mut args, "--force", p.force);
        push_flag(&mut args, "--dry-run", p.dry_run);
        push_flag(&mut args, "--no-build", p.no_build);
        push_opt(&mut args, "--api-key", p.api_key);
        push_opt(&mut args, "--zai-api-key", p.zai_api_key);
        push_flag(&mut args, "--no-pbr", p.no_pbr);
        push_flag(&mut args, "--no-normal", p.no_normal);
        push_flag(
            &mut args,
            "--no-metallic-roughness",
            p.no_metallic_roughness,
        );
        push_flag(&mut args, "--no-occlusion", p.no_occlusion);
        push_opt(&mut args, "--texture-size", p.texture_size);
        run_mogen(args).await
    }

    // -- ops / bench --------------------------------------------------------

    #[tool(
        description = "Download the latest release from GitHub and replace the running mogen \
        binary in place. Without `yes=true`, only checks and prints what it would do."
    )]
    async fn update(
        &self,
        Parameters(p): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["update".to_string()];
        push_flag(&mut args, "--yes", p.yes);
        push_flag(&mut args, "--check", p.check);
        push_flag(&mut args, "--force", p.force);
        run_mogen(args).await
    }

    #[tool(
        description = "Run a suite of prompts through `generate` and report success rate and \
        mean token cost. Does not write GLBs."
    )]
    async fn bench(
        &self,
        Parameters(p): Parameters<BenchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["bench".to_string()];
        push_path_opt(&mut args, "--prompts", p.prompts);
        push_opt(&mut args, "--provider", p.provider);
        push_opt(&mut args, "--model", p.model);
        push_opt(&mut args, "--thinking", p.thinking);
        push_opt(&mut args, "--max-repair-iters", p.max_repair_iters);
        push_opt(&mut args, "--budget-tokens", p.budget_tokens);
        push_opt(&mut args, "--api-key", p.api_key);
        push_flag(&mut args, "--no-cache", p.no_cache);
        run_mogen(args).await
    }

    // -- moghub -------------------------------------------------------------

    #[tool(description = "Print the signed-in MoGHub user's handle and id. Fails if no session.")]
    async fn moghub_whoami(
        &self,
        Parameters(p): Parameters<ServerOnly>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "whoami".to_string()];
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "Browse the public MoGHub discover feed.")]
    async fn moghub_discover(
        &self,
        Parameters(p): Parameters<MoghubDiscoverArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "discover".to_string()];
        push_opt(&mut args, "--query", p.query);
        push_opt(&mut args, "--kind", p.kind);
        push_opt(&mut args, "--tag", p.tag);
        push_opt(&mut args, "--limit", p.limit);
        push_opt(&mut args, "--offset", p.offset);
        push_flag(&mut args, "--json", p.json);
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(
        description = "Print full detail for a MoGHub model. Reference is `<user>/<slug>` \
        or `@<user>/<slug>`."
    )]
    async fn moghub_info(
        &self,
        Parameters(p): Parameters<MoghubRefArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "info".to_string(), p.reference];
        push_flag(&mut args, "--json", p.json);
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(
        description = "Download a MoGHub model's `.mog` files into a directory. Defaults to \
        the latest version."
    )]
    async fn moghub_download(
        &self,
        Parameters(p): Parameters<MoghubDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "download".to_string(), p.reference];
        push_opt(&mut args, "--version", p.version);
        push_path_opt(&mut args, "--out", p.out);
        push_flag(&mut args, "--entry-only", p.entry_only);
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "List comments on a MoGHub model.")]
    async fn moghub_comments(
        &self,
        Parameters(p): Parameters<MoghubRefOnly>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "comments".to_string(), p.reference];
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "Post a comment on a MoGHub model. Body accepts BBCode.")]
    async fn moghub_comment(
        &self,
        Parameters(p): Parameters<MoghubCommentArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "moghub".to_string(),
            "comment".to_string(),
            p.reference,
            p.body,
        ];
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "Like a MoGHub model. Idempotent.")]
    async fn moghub_like(
        &self,
        Parameters(p): Parameters<MoghubRefOnly>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "like".to_string(), p.reference];
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "Remove a previously-set like on a MoGHub model.")]
    async fn moghub_unlike(
        &self,
        Parameters(p): Parameters<MoghubRefOnly>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "unlike".to_string(), p.reference];
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(description = "List the signed-in user's MoGHub notifications.")]
    async fn moghub_notifications(
        &self,
        Parameters(p): Parameters<MoghubNotificationsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["moghub".to_string(), "notifications".to_string()];
        push_flag(&mut args, "--mark-read", p.mark_read);
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }

    #[tool(
        description = "Publish a `.mog` to MoGHub. Bundles every locally imported `.mog` \
        plus referenced PNG/JPG/JPEG/WebP textures. Re-publishing a file with a prior MoGHub \
        stamp appends a version unless `new=true`."
    )]
    async fn moghub_publish(
        &self,
        Parameters(p): Parameters<MoghubPublishArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "moghub".to_string(),
            "publish".to_string(),
            p.input.to_string_lossy().into_owned(),
        ];
        push_opt(&mut args, "--title", p.title);
        push_opt(&mut args, "--description", p.description);
        push_opt(&mut args, "--tags", p.tags);
        push_opt(&mut args, "--license", p.license);
        push_opt(&mut args, "--visibility", p.visibility);
        push_opt(&mut args, "--message", p.message);
        push_path_opt(&mut args, "--thumbnail", p.thumbnail);
        push_opt(&mut args, "--filename", p.filename);
        push_flag(&mut args, "--module", p.module);
        push_flag(&mut args, "--scene", p.scene);
        push_flag(&mut args, "--new", p.new);
        push_opt(&mut args, "--server", p.server);
        run_mogen(args).await
    }
}

#[tool_handler]
impl ServerHandler for MogenMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::V_2024_11_05;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        // `Implementation::from_build_env()` reads `CARGO_*` env vars at
        // the rmcp crate's compile site, so it reports "rmcp" — build
        // one ourselves so clients see "mogen" instead.
        info.server_info = Implementation::from_build_env();
        info.server_info.name = env!("CARGO_PKG_NAME").to_string();
        info.server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = info.server_info.with_title("MoGen").with_description(
            "Procedural 3D model generator. Compiles `.mog` DSL to glTF/FBX and \
                drives an LLM-backed generation pipeline.",
        );
        info.instructions = Some(
            "MoGen MCP server. Exposes every `mogen` CLI subcommand as a tool. \
            Use `build` / `check` / `parse` / `dump_scene` / `inspect` / `thumbnail` for \
            deterministic compile + introspection of `.mog` files; `generate` / `modify` / \
            `animate` / `repair` / `textures` to drive mogen's own LLM pipeline; \
            `moghub_*` for community browse + publish; `update` / `bench` for ops. \
            Every tool spawns the same `mogen` binary as a subprocess and returns its \
            stdout + stderr. Paths are relative to the server's working directory."
                .to_string(),
        );
        info
    }
}
