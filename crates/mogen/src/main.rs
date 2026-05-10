mod cli;
mod commands;
mod common;
mod format;
mod refine_render;
mod spinner;

use std::process::ExitCode;

use clap::Parser;

use cli::{dispatch_moghub, BuildFormatArg, Cmd};
use commands::animate::{animate, AnimateArgs};
use commands::auth::dispatch as auth_dispatch;
use commands::bench::bench;
use commands::build::build;
use commands::generate::{generate, GenerateArgs};
use commands::inspect::{check, dump_scene, inspect, parse_cmd};
use commands::modify::{modify, ModifyArgs};
use commands::repair::{repair, RepairArgs};
use commands::textures::textures_cmd;
use commands::thumbnail::{thumbnail, ThumbnailArgs};
use commands::update::{apply_update, update, UpdateArgs};

#[derive(Parser)]
#[command(name = "mogen", version, about = "Procedural 3D model generator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
            style,
            plan,
            auto_refine,
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
            style: style.map(Into::into),
            plan,
            auto_refine,
            auth: provider.into(),
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
            style,
            plan,
            auto_refine,
            rewrite,
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
            style: style.map(Into::into),
            plan,
            auto_refine,
            rewrite,
            auth: provider.into(),
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
            style,
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
            style: style.map(Into::into),
            auth: provider.into(),
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
            style,
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
            style: style.map(Into::into),
            auth: provider.into(),
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
        Cmd::ApplyUpdate { plan } => apply_update(plan),
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
            provider.into(),
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
