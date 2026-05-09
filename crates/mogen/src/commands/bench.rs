use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use mogen_llm::{
    cacheable_block, default_cache_path, generate_with_repair, inline_block,
    resolve_or_create_cache, system_instruction, GenerateConfig, Provider, RepairConfig,
    StdlibIndex, ThinkingLevel, DEFAULT_TTL_SECONDS,
};

use crate::common::{build_llm_client, resolve_model, GeminiAuthMode};

pub(crate) fn bench(
    prompts_path: PathBuf,
    provider: Provider,
    model: Option<String>,
    max_repair_iters: u32,
    budget_tokens: Option<u32>,
    api_key: Option<String>,
    no_cache: bool,
    thinking: ThinkingLevel,
    auth: GeminiAuthMode,
) -> Result<()> {
    let client = build_llm_client(provider, api_key, auth)?;
    let model = resolve_model(provider, model);

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
    // Only Gemini honours `cachedContents`; on other providers we fall back
    // to inline.
    //
    // Post-split (see `mogen_llm::prompt`): the cache holds `cacheable_block`
    // (grammar + kinds + allowlist, ~17 KB) and each per-prompt request
    // carries `inline_block(idx)` fresh as `systemInstruction`. When the
    // cache is unavailable, fall back to the full `system_instruction` inline.
    let idx = StdlibIndex::from_registry(mogen_dsl::stdlib_registry());
    let inline = inline_block(&idx);
    let full_inline = system_instruction(&idx);
    let cached_name: Option<String> = if no_cache {
        None
    } else if let Some(gemini) = client.as_gemini() {
        if let Some(cache_path) = default_cache_path() {
            let cacheable = cacheable_block();
            match resolve_or_create_cache(
                gemini,
                &model,
                &cacheable,
                &cache_path,
                DEFAULT_TTL_SECONDS,
            ) {
                Ok(name) => Some(name),
                Err(e) => {
                    eprintln!(
                        "mogen bench: cache unavailable ({e}); sending system instruction inline"
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let mut successes = 0usize;
    let mut total_tokens = 0u64;
    let mut total_calls = 0u32;

    println!(
        "# mogen bench — {} prompts, provider={}, model={}, max_repair_iters={}, cache={}",
        prompts.len(),
        provider.key(),
        model,
        max_repair_iters,
        if cached_name.is_some() { "on" } else { "off" },
    );

    for (i, prompt) in prompts.iter().enumerate() {
        let mut cfg = GenerateConfig::new(*prompt);
        cfg.model = model.clone();
        if let Some(name) = &cached_name {
            cfg.cached_content = Some(name.clone());
            cfg.system_instruction = Some(inline.clone());
        } else {
            cfg.system_instruction = Some(full_inline.clone());
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
                        .filter(|d| matches!(d.severity, mogen_core::Severity::Error))
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
