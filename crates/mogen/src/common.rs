use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use mogen_core::{Diagnostic, Severity};
use mogen_llm::google_oauth::{ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};
use mogen_llm::{
    cacheable_block, default_cache_path, inline_block, load_bundle, read_api_key,
    resolve_or_create_cache, system_instruction, token_store_path, token_store_path_for,
    GenerateConfig, GoogleCredential, LlmClient, Provider, StdlibIndex, DEFAULT_TTL_SECONDS,
};

/// Gemini-specific credential selector — controls which credential path
/// `--provider` resolves to when the active provider is Gemini. Folded
/// into the CLI's `--provider` flag itself: `--provider auto`,
/// `--provider gemini`, `--provider gemini-oauth`, and
/// `--provider antigravity` all set [`Provider::Gemini`] and pick a mode
/// here. Non-Gemini providers ignore this enum.
///
/// - `Auto` (default) preserves the historical resolution order — explicit
///   flag → env → settings.json → gemini-cli OAuth — and adds an
///   Antigravity OAuth fallback when the gemini-cli bundle fails to load.
///   This unblocks users whose gemini-cli OAuth client is rejected with 403
///   while their Antigravity bundle still works.
/// - `ApiKey` forces the API-key path (flag → env → settings.json) and
///   skips OAuth entirely.
/// - `GeminiOauth` forces the gemini-cli OAuth bundle.
/// - `Antigravity` forces the Antigravity OAuth bundle (the same client
///   `mogen textures` uses for image generation; also valid for text gen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GeminiAuthMode {
    #[default]
    Auto,
    ApiKey,
    GeminiOauth,
    Antigravity,
}

/// Group error diagnostics by category (derived from the code prefix) so the
/// repair spinner can say "fixing 2 syntax, 1 attach" instead of just "3".
pub(crate) fn summarize_repair_errors(diags: &[Diagnostic]) -> String {
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
pub(crate) fn diag_category(code: &str) -> &'static str {
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

/// Resolve the API key for `provider`. Precedence: explicit `--api-key`
/// flag → provider env var ([`Provider::env_var`]) →
/// `~/.mogen/settings.json` (shared with Studio). Ollama and Claude Code
/// are allowed to start with a blank key — most local installs don't
/// require auth.
pub(crate) fn resolve_api_key(provider: Provider, flag: Option<String>) -> Result<String> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(k);
    }
    if provider.is_keyless() {
        // Ollama (local) and Claude Code (subscription via `claude` CLI)
        // are keyless by default — fall through with empty string so the
        // client constructs without auth. An env var or settings entry is
        // still consulted for users running Ollama behind an
        // authenticating reverse proxy.
        let var = provider.env_var();
        if !var.is_empty() {
            let from_env = std::env::var(var).unwrap_or_default();
            if !from_env.trim().is_empty() {
                return Ok(from_env);
            }
        }
        if let Some(k) = read_api_key(provider) {
            return Ok(k);
        }
        return Ok(String::new());
    }
    let var = provider.env_var();
    let from_env = std::env::var(var).unwrap_or_default();
    if !from_env.trim().is_empty() {
        return Ok(from_env);
    }
    if let Some(k) = read_api_key(provider) {
        return Ok(k);
    }
    bail!("missing {var} (set env var, pass --api-key, or store it in ~/.mogen/settings.json)");
}

/// Resolve the model id for the call. Falls back to the provider-specific
/// default when the user passed nothing.
pub(crate) fn resolve_model(provider: Provider, flag: Option<String>) -> String {
    flag.filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| provider.default_model().to_string())
}

/// Construct the right [`LlmClient`] for `provider` with `api_key`.
///
/// Z.ai routes through `with_base_url` so the persisted GLM Coding Plan
/// toggle (in `~/.mogen/settings.json`) chooses between the dedicated
/// `/api/coding/paas/v4` endpoint (default-on, recommended for coding-plan
/// keys) and the general `/api/paas/v4` surface.
pub(crate) fn build_client(provider: Provider, api_key: String) -> LlmClient {
    if provider == Provider::Zai {
        LlmClient::with_base_url(provider, api_key, mogen_llm::zai_base_url())
    } else {
        LlmClient::new(provider, api_key)
    }
}

/// Resolve the Google credential for Gemini calls under the given `mode`.
///
/// - [`GeminiAuthMode::Auto`] (default): `--api-key` flag → `GEMINI_API_KEY`
///   env → `~/.mogen/settings.json` → gemini-cli OAuth bundle → Antigravity
///   OAuth bundle (fallback). API-key beats stored OAuth deliberately so
///   users can always force the public-API path when both are configured
///   (`mogen auth status` flags this shadowing). The Antigravity fallback
///   was added because Google began rejecting some gemini-cli OAuth clients
///   with 403 even though the bundle still loads.
/// - [`GeminiAuthMode::ApiKey`]: only the API-key chain (flag → env →
///   settings.json). Errors out rather than falling back to OAuth.
/// - [`GeminiAuthMode::GeminiOauth`]: only the gemini-cli OAuth bundle.
/// - [`GeminiAuthMode::Antigravity`]: only the Antigravity OAuth bundle.
pub(crate) fn resolve_gemini_credential(
    flag: Option<String>,
    mode: GeminiAuthMode,
) -> Result<GoogleCredential> {
    match mode {
        GeminiAuthMode::Auto => resolve_gemini_credential_auto(flag),
        GeminiAuthMode::ApiKey => resolve_gemini_credential_api_key(flag),
        GeminiAuthMode::GeminiOauth => {
            if flag.is_some() {
                bail!("--provider gemini-oauth ignores --api-key; drop one of the flags");
            }
            load_gemini_cli_bundle()
        }
        GeminiAuthMode::Antigravity => {
            if flag.is_some() {
                bail!("--provider antigravity ignores --api-key; drop one of the flags");
            }
            load_antigravity_bundle()
        }
    }
}

fn resolve_gemini_credential_auto(flag: Option<String>) -> Result<GoogleCredential> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(GoogleCredential::ApiKey(k));
    }
    let from_env = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    if !from_env.trim().is_empty() {
        return Ok(GoogleCredential::ApiKey(from_env));
    }
    if let Some(k) = read_api_key(Provider::Gemini) {
        return Ok(GoogleCredential::ApiKey(k));
    }
    // Try the gemini-cli OAuth bundle first; on any I/O / parse error fall
    // through to Antigravity rather than burning the whole resolution.
    if let Some(path) = token_store_path() {
        match load_bundle(&path) {
            Ok(Some(bundle)) => return Ok(GoogleCredential::OAuth(bundle)),
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "mogen: stored OAuth credentials at {} unreadable ({e}); ignoring",
                    path.display()
                );
            }
        }
    }
    // Fallback: Antigravity bundle works for text gen too. This rescues
    // users whose gemini-cli client returns 403 while their Antigravity
    // login is still healthy. Pass `--auth gemini-oauth` to skip this
    // fallback explicitly.
    if let Some(path) = token_store_path_for(&ANTIGRAVITY_CONFIG) {
        match load_bundle(&path) {
            Ok(Some(bundle)) => {
                eprintln!(
                    "mogen: gemini-cli OAuth bundle missing or unreadable; \
                     using Antigravity OAuth bundle instead. Pass \
                     `--auth gemini-oauth` to disable this fallback."
                );
                return Ok(GoogleCredential::AntigravityOAuth(bundle));
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "mogen: stored Antigravity OAuth credentials at {} unreadable ({e}); ignoring",
                    path.display()
                );
            }
        }
    }
    bail!(
        "missing GEMINI_API_KEY (set env var, pass --api-key, store it in ~/.mogen/settings.json, or run 'mogen auth login')"
    );
}

fn resolve_gemini_credential_api_key(flag: Option<String>) -> Result<GoogleCredential> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(GoogleCredential::ApiKey(k));
    }
    let from_env = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    if !from_env.trim().is_empty() {
        return Ok(GoogleCredential::ApiKey(from_env));
    }
    if let Some(k) = read_api_key(Provider::Gemini) {
        return Ok(GoogleCredential::ApiKey(k));
    }
    bail!(
        "missing GEMINI_API_KEY (set env var, pass --api-key, or store it in ~/.mogen/settings.json) — \
         --auth api-key disables OAuth fallbacks"
    );
}

fn load_gemini_cli_bundle() -> Result<GoogleCredential> {
    let path = token_store_path_for(&GEMINI_CLI_CONFIG)
        .context("could not locate the gemini-cli token store path")?;
    match load_bundle(&path)
        .with_context(|| format!("reading gemini-cli OAuth bundle at {}", path.display()))?
    {
        Some(bundle) => Ok(GoogleCredential::OAuth(bundle)),
        None => bail!(
            "no gemini-cli OAuth bundle at {} — run `mogen auth login` first",
            path.display()
        ),
    }
}

fn load_antigravity_bundle() -> Result<GoogleCredential> {
    let path = token_store_path_for(&ANTIGRAVITY_CONFIG)
        .context("could not locate the Antigravity token store path")?;
    match load_bundle(&path)
        .with_context(|| format!("reading Antigravity OAuth bundle at {}", path.display()))?
    {
        Some(bundle) => Ok(GoogleCredential::AntigravityOAuth(bundle)),
        None => bail!(
            "no Antigravity OAuth bundle at {} — run `mogen auth login --antigravity` first",
            path.display()
        ),
    }
}

/// Resolve a Google credential suitable for **image generation** (the
/// nano-banana / `gemini-3-pro-image` surface).
///
/// Precedence: `--api-key` flag → `GEMINI_API_KEY` env → on-disk
/// **Antigravity** OAuth bundle → error.
///
/// The plain `mogen auth login` (gemini-cli) bundle is intentionally *not*
/// accepted here — that OAuth client is rejected by the image surface with
/// a 403 "caller does not have permission". Instead, when only the
/// gemini-cli bundle is present we surface a clear "run `mogen auth login
/// --antigravity`" error so the user can authorise the image-capable
/// client without losing their existing text-gen login.
pub(crate) fn resolve_gemini_image_credential(
    flag: Option<String>,
) -> Result<GoogleCredential> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(GoogleCredential::ApiKey(k));
    }
    let from_env = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    if !from_env.trim().is_empty() {
        return Ok(GoogleCredential::ApiKey(from_env));
    }
    if let Some(k) = read_api_key(Provider::Gemini) {
        return Ok(GoogleCredential::ApiKey(k));
    }
    if let Some(path) = token_store_path_for(&ANTIGRAVITY_CONFIG) {
        match load_bundle(&path) {
            Ok(Some(bundle)) => {
                return Ok(GoogleCredential::AntigravityOAuth(bundle));
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!(
                    "mogen: stored Antigravity OAuth credentials at {} unreadable ({e}); ignoring",
                    path.display()
                );
            }
        }
    }
    let has_gemini_cli = token_store_path_for(&GEMINI_CLI_CONFIG)
        .and_then(|p| load_bundle(&p).ok().flatten())
        .is_some();
    if has_gemini_cli {
        bail!(
            "image generation requires the Antigravity OAuth client. \
             Your current `mogen auth login` bundle uses the gemini-cli \
             client, which the image surface rejects with 403. \
             Run `mogen auth login --antigravity` (or set GEMINI_API_KEY) \
             and try again."
        );
    }
    bail!(
        "missing GEMINI_API_KEY (set env var, pass --api-key, or run \
         `mogen auth login --antigravity` for image generation over OAuth)"
    );
}

/// Build the right [`LlmClient`] for `provider` under `auth`.
///
/// For Gemini, threads `flag` + `auth` through [`resolve_gemini_credential`]
/// so callers can choose between API key, gemini-cli OAuth, Antigravity
/// OAuth, or the Auto fallback chain. For non-Gemini providers, `auth` is
/// constrained: only [`GeminiAuthMode::Auto`] / [`GeminiAuthMode::ApiKey`]
/// are valid; passing [`GeminiAuthMode::GeminiOauth`] /
/// [`GeminiAuthMode::Antigravity`] errors so the user notices the mismatch
/// instead of silently getting an API-key auth they didn't ask for.
pub(crate) fn build_llm_client(
    provider: Provider,
    flag: Option<String>,
    auth: GeminiAuthMode,
) -> Result<LlmClient> {
    if matches!(provider, Provider::Gemini) {
        let cred = resolve_gemini_credential(flag, auth)?;
        return Ok(LlmClient::gemini_from_credential(cred));
    }
    match auth {
        GeminiAuthMode::Auto | GeminiAuthMode::ApiKey => {
            // Non-Gemini providers always go through the API-key chain.
        }
        GeminiAuthMode::GeminiOauth => bail!(
            "--provider gemini-oauth is Gemini-only (got --provider {})",
            provider.label()
        ),
        GeminiAuthMode::Antigravity => bail!(
            "--provider antigravity is Gemini-only (got --provider {})",
            provider.label()
        ),
    }
    let key = resolve_api_key(provider, flag)?;
    Ok(build_client(provider, key))
}

/// Create the parent directory for `path` if it doesn't already exist. Called
/// before any expensive work (like an LLM round-trip) so path errors surface up
/// front instead of after tokens are spent.
pub(crate) fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    Ok(())
}

/// Set `cfg.cached_content` and/or `cfg.system_instruction` based on flags.
///
/// The system instruction is split (see `mogen_llm::prompt`):
///   - `cacheable_block()` — grammar + kinds + attribute allowlist (~17 KB,
///     stable across all builds at a fixed grammar/validator version).
///   - `inline_block(idx)` — preamble + conventions + fewshots + stdlib
///     summary + output contract (~22 KB, sent fresh per request).
///
/// Precedence:
///   1. `--cached-content <name>` — pin that resource (Gemini only). We pair
///      it with `inline_block` per request, on the assumption the pinned
///      resource was created from `cacheable_block` (post-split). Old caches
///      created from the full instruction will produce duplicated content
///      until they expire — run `--no-cache` once or wait for TTL.
///   2. `--no-cache`, or non-Gemini provider — send the full
///      `system_instruction` inline (no caching benefit available).
///   3. Default (Gemini only) — resolve or create a persistent cache entry
///      keyed by `cacheable_block` content under `$MOGEN_CACHE_DIR` /
///      `$HOME/.cache/mogen/`, send `inline_block` fresh per request. Fall
///      back to full inline on any failure (warned to stderr).
pub(crate) fn attach_system_instruction(
    cfg: &mut GenerateConfig,
    client: &LlmClient,
    pinned: Option<String>,
    no_cache: bool,
    label: &str,
) {
    let idx = StdlibIndex::from_registry(mogen_dsl::stdlib_registry());

    // Non-Gemini providers don't honour `cachedContents`. Force inline.
    let gemini = match client {
        LlmClient::Gemini(c) => Some(c),
        _ => None,
    };

    if let Some(name) = pinned {
        if gemini.is_some() {
            cfg.cached_content = Some(name);
            cfg.system_instruction = Some(inline_block(&idx));
        } else {
            // Politely warn and fall through — the flag is silently ignored
            // on backends that have no equivalent feature.
            eprintln!(
                "mogen {label}: --cached-content is Gemini-only; sending system instruction inline"
            );
            cfg.system_instruction = Some(system_instruction(&idx));
        }
        return;
    }
    if no_cache || gemini.is_none() {
        cfg.system_instruction = Some(system_instruction(&idx));
        return;
    }

    let gemini = gemini.expect("matched above");
    let Some(cache_path) = default_cache_path() else {
        cfg.system_instruction = Some(system_instruction(&idx));
        return;
    };

    let cacheable = cacheable_block();
    match resolve_or_create_cache(
        gemini,
        &cfg.model,
        &cacheable,
        &cache_path,
        DEFAULT_TTL_SECONDS,
    ) {
        Ok(name) => {
            cfg.cached_content = Some(name);
            cfg.system_instruction = Some(inline_block(&idx));
        }
        Err(e) => {
            eprintln!(
                "mogen {label}: cache unavailable ({e}); sending system instruction inline"
            );
            cfg.system_instruction = Some(system_instruction(&idx));
        }
    }
}

/// Render the cached-token portion of a usage record. Returns an empty
/// string when the call didn't hit a cache, so the summary doesn't grow a
/// noisy "cached=0" suffix for inline runs.
pub(crate) fn format_cached_tokens(usage: &mogen_llm::Usage) -> String {
    if usage.cached_tokens > 0 {
        format!(", cached={}", usage.cached_tokens)
    } else {
        String::new()
    }
}

/// Deterministic-ish seed from the current time. Stable seeds come from --seed.
pub(crate) fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}
