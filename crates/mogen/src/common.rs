use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use mogen_core::{Diagnostic, Severity};
use mogen_llm::{
    default_cache_path, resolve_or_create_cache, system_instruction, GenerateConfig, LlmClient,
    Provider, StdlibIndex, DEFAULT_TTL_SECONDS,
};

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

/// Resolve the API key for `provider`. Precedence: explicit `--api-key` flag,
/// then the provider's environment variable ([`Provider::env_var`]). Ollama
/// is allowed to start with a blank key — most local installs don't require
/// auth.
pub(crate) fn resolve_api_key(provider: Provider, flag: Option<String>) -> Result<String> {
    if let Some(k) = flag {
        if k.trim().is_empty() {
            bail!("--api-key is empty");
        }
        return Ok(k);
    }
    if provider.is_keyless() {
        // Ollama (local) and Claude Code (subscription via `claude` CLI) are
        // keyless by default — fall through with empty string so the client
        // constructs without auth. An env var is still consulted for users
        // running Ollama behind an authenticating reverse proxy.
        let var = provider.env_var();
        if !var.is_empty() {
            let from_env = std::env::var(var).unwrap_or_default();
            if !from_env.trim().is_empty() {
                return Ok(from_env);
            }
        }
        return Ok(String::new());
    }
    let var = provider.env_var();
    let from_env = std::env::var(var).unwrap_or_default();
    if from_env.trim().is_empty() {
        bail!("missing {var} (set env var or pass --api-key)");
    }
    Ok(from_env)
}

/// Resolve the model id for the call. Falls back to the provider-specific
/// default when the user passed nothing.
pub(crate) fn resolve_model(provider: Provider, flag: Option<String>) -> String {
    flag.filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| provider.default_model().to_string())
}

/// Construct the right [`LlmClient`] for `provider` with `api_key`.
pub(crate) fn build_client(provider: Provider, api_key: String) -> LlmClient {
    LlmClient::new(provider, api_key)
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

/// Set `cfg.cached_content` or `cfg.system_instruction` based on flags.
///
/// Precedence:
///   1. `--cached-content <name>` — use that resource name verbatim (Gemini
///      only; ignored on other providers).
///   2. `--no-cache`, or non-Gemini provider — send the system instruction
///      inline.
///   3. Default (Gemini only) — try to resolve or create a persistent cache
///      entry under `$MOGEN_CACHE_DIR` / `$HOME/.cache/mogen/`, falling back
///      to inline on any failure (printed to stderr so the user notices
///      repeat failures).
pub(crate) fn attach_system_instruction(
    cfg: &mut GenerateConfig,
    client: &LlmClient,
    pinned: Option<String>,
    no_cache: bool,
    label: &str,
) {
    let system_text = system_instruction(&StdlibIndex::from_registry(
        mogen_dsl::stdlib_registry(),
    ));

    // Non-Gemini providers don't honour `cachedContents`. Force inline.
    let gemini = match client {
        LlmClient::Gemini(c) => Some(c),
        _ => None,
    };

    if let Some(name) = pinned {
        if gemini.is_some() {
            cfg.cached_content = Some(name);
        } else {
            // Politely warn and fall through — the flag is silently ignored
            // on backends that have no equivalent feature.
            eprintln!(
                "mogen {label}: --cached-content is Gemini-only; sending system instruction inline"
            );
            cfg.system_instruction = Some(system_text);
        }
        return;
    }
    if no_cache || gemini.is_none() {
        cfg.system_instruction = Some(system_text);
        return;
    }

    let gemini = gemini.expect("matched above");
    let Some(cache_path) = default_cache_path() else {
        cfg.system_instruction = Some(system_text);
        return;
    };

    match resolve_or_create_cache(
        gemini,
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
                "mogen {label}: cache unavailable ({e}); sending system instruction inline"
            );
            cfg.system_instruction = Some(system_text);
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
