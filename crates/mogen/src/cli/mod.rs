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
