//! `clap`-derived CLI surface for the `mogen` binary. Split from `main.rs`
//! so the entry point stays focused on dispatch — every flag enum, every
//! subcommand, and the moghub/auth conversion glue lives here.

mod auth;
mod cmd;
mod moghub;
mod value_args;

pub(crate) use cmd::Cmd;
pub(crate) use moghub::dispatch_moghub;
pub(crate) use value_args::BuildFormatArg;

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn text_commands_default_to_openai_and_keep_gemini_auth_modes() {
        let cli = crate::Cli::command();
        for name in ["generate", "modify", "animate", "repair", "bench"] {
            let command = cli.find_subcommand(name).unwrap();
            let provider = command
                .get_arguments()
                .find(|arg| arg.get_id() == "provider")
                .unwrap();
            assert_eq!(
                provider.get_default_values(),
                &[std::ffi::OsString::from("openai")],
                "{name}"
            );
            let choices: Vec<_> = provider
                .get_possible_values()
                .into_iter()
                .map(|value| value.get_name().to_string())
                .collect();
            for choice in [
                "openai",
                "codex",
                "gemini",
                "auto",
                "gemini-oauth",
                "antigravity",
            ] {
                assert!(
                    choices.iter().any(|value| value == choice),
                    "{name}: missing {choice}"
                );
            }
        }
    }
}

pub(crate) use value_args::ProviderArg;
