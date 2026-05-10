//! `mogen moghub …` subcommand surface — the terminal mirror of the
//! Studio's Community window. Read-only verbs work without a session;
//! write verbs require `mogen auth moghub login`.

use std::path::PathBuf;

use clap::Subcommand;

use crate::commands::moghub::{
    self as moghub_cmd, DiscoverArgs as MoghubDiscoverArgs, DownloadArgs as MoghubDownloadArgs,
    PublishArgs as MoghubPublishArgs,
};

/// Subcommands under `mogen moghub`. Read-only verbs work without a
/// session; write verbs (`publish`, `comment`, `like`/`unlike`,
/// `notifications`) require `mogen auth moghub login` to have stored
/// a session token.
#[derive(Subcommand)]
pub(crate) enum MoghubCmd {
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

pub(crate) fn dispatch_moghub(cmd: MoghubCmd) -> anyhow::Result<()> {
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
