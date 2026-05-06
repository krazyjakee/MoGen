//! `mogen moghub` — community publishing + browsing surface that
//! mirrors what Studio's Community window can do.
//!
//! Auth comes from the same `~/.mogen/moghub_auth.json` file Studio
//! uses, so logging in via `mogen auth moghub login` covers both. The
//! base URL is taken from the auth file (or `--server`); production is
//! `https://moghub.org`.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_moghub_client::{
    session_store as moghub_session, DiscoverQuery, MoghubClient, DEFAULT_BASE_URL,
};

pub(crate) use super::moghub_publish::{publish, PublishArgs};

/// Commands gated to the signed-in user. Public read-only commands
/// (discover/info/download/comments) work without a token.
fn require_token() -> Result<String> {
    moghub_session::read_session()
        .ok_or_else(|| anyhow!("not logged in to moghub. Run 'mogen auth moghub login' first."))
}

fn resolve_base_url(server: Option<String>) -> String {
    server
        .or_else(moghub_session::read_base_url)
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

fn client(server: Option<String>, with_auth: bool) -> Result<MoghubClient> {
    let base_url = resolve_base_url(server);
    let mut c = MoghubClient::new(&base_url)
        .with_context(|| format!("constructing moghub client for {base_url}"))?;
    if with_auth {
        let token = require_token()?;
        c = c.with_token(Some(token));
    } else if let Some(token) = moghub_session::read_session() {
        // Read-only endpoints accept an optional bearer; sending it lets
        // the server tag `liked_by_me` and surface unlisted models the
        // user owns.
        c = c.with_token(Some(token));
    }
    Ok(c)
}

/// Parse `@user/slug` or `user/slug` into `(user, slug)`.
fn parse_user_slug(s: &str) -> Result<(String, String)> {
    let trimmed = s.trim().trim_start_matches('@');
    let mut parts = trimmed.splitn(2, '/');
    let user = parts.next().unwrap_or("").trim();
    let slug = parts.next().unwrap_or("").trim();
    if user.is_empty() || slug.is_empty() {
        bail!(
            "expected `<user>/<slug>` (or `@<user>/<slug>`), got `{}`",
            s
        );
    }
    Ok((user.to_string(), slug.to_string()))
}

pub(crate) fn whoami(server: Option<String>) -> Result<()> {
    let c = client(server, false)?;
    match c.whoami()?.user {
        Some(u) => println!("{} (id {})", u.handle, u.id),
        None => {
            println!("anonymous");
            std::process::exit(1);
        }
    }
    Ok(())
}

pub(crate) struct DiscoverArgs {
    pub query: Option<String>,
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub json: bool,
    pub server: Option<String>,
}

pub(crate) fn discover(args: DiscoverArgs) -> Result<()> {
    let c = client(args.server, false)?;
    let resp = c.discover(DiscoverQuery {
        q: args.query,
        kind: args.kind,
        tag: args.tag,
        limit: args.limit,
        offset: args.offset,
    })?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    if let Some(featured) = resp.featured.as_ref() {
        println!(
            "★ @{}/{}  {}  [{}]",
            featured.user.handle, featured.slug, featured.title, featured.kind,
        );
    }
    if resp.items.is_empty() {
        println!("(no results)");
    }
    for m in &resp.items {
        let tags = if m.tags.is_empty() {
            String::new()
        } else {
            format!("  #{}", m.tags.join(" #"))
        };
        println!(
            "@{}/{}  {}  [{}]  ♥{}{}",
            m.user.handle, m.slug, m.title, m.kind, m.like_count, tags,
        );
    }
    Ok(())
}

pub(crate) fn info(reference: String, json: bool, server: Option<String>) -> Result<()> {
    let (user, slug) = parse_user_slug(&reference)?;
    let c = client(server, false)?;
    let detail = c.model_detail(&user, &slug)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&detail)?);
        return Ok(());
    }
    println!("@{}/{}  {}", detail.user.handle, detail.slug, detail.title);
    println!(
        "  kind={}  license={}  ♥{}  forks={}  v{}",
        detail.kind, detail.license, detail.like_count, detail.fork_count, detail.version.version,
    );
    if !detail.description.trim().is_empty() {
        println!("\n{}\n", detail.description.trim());
    }
    if !detail.tags.is_empty() {
        println!("tags: #{}", detail.tags.join(" #"));
    }
    println!("files:");
    for f in &detail.version.files {
        let marker = if f.is_entry { "→" } else { " " };
        println!("  {marker} {}  ({} bytes)", f.filename, f.bytes);
    }
    Ok(())
}

pub(crate) struct DownloadArgs {
    pub reference: String,
    pub version: Option<i32>,
    pub out: Option<PathBuf>,
    pub entry_only: bool,
    pub server: Option<String>,
}

pub(crate) fn download(args: DownloadArgs) -> Result<()> {
    let (user, slug) = parse_user_slug(&args.reference)?;
    let c = client(args.server, false)?;

    // version_detail returns inline file bodies for any pinned version;
    // model_detail does the same for the latest. Pick whichever matches
    // the user's --version flag.
    let (files, version_id, picked_version) = match args.version {
        Some(v) => {
            let d = c.version_detail(&user, &slug, v)?;
            (d.version.files, d.version.id, d.version.version)
        }
        None => {
            let d = c.model_detail(&user, &slug)?;
            (d.version.files, d.version.id, d.version.version)
        }
    };

    let out_dir = args.out.unwrap_or_else(|| PathBuf::from(format!("{slug}-v{picked_version}")));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;

    let mut written = 0;
    for f in &files {
        if args.entry_only && !f.is_entry {
            continue;
        }
        let dest = out_dir.join(&f.filename);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&dest, &f.source)
            .with_context(|| format!("writing {}", dest.display()))?;
        written += 1;
    }

    // Best-effort thumbnail. The endpoint 404s for never-thumbnailed
    // versions, which we treat as a no-op.
    if !args.entry_only {
        if let Ok(bytes) = c.thumbnail_png(&user, &slug, &version_id) {
            let dest = out_dir.join("thumbnail.png");
            std::fs::write(&dest, bytes)
                .with_context(|| format!("writing {}", dest.display()))?;
            written += 1;
        }
    }

    println!(
        "Downloaded {written} file{} from @{user}/{slug} v{picked_version} to {}",
        if written == 1 { "" } else { "s" },
        out_dir.display(),
    );
    Ok(())
}

pub(crate) fn comments(reference: String, server: Option<String>) -> Result<()> {
    let (user, slug) = parse_user_slug(&reference)?;
    let c = client(server, false)?;
    let list = c.comments(&user, &slug)?;
    if list.comments.is_empty() {
        println!("(no comments)");
        return Ok(());
    }
    for cm in &list.comments {
        if cm.deleted {
            continue;
        }
        println!("@{}  {}", cm.user.handle, cm.created_at);
        for line in cm.body.lines() {
            println!("  {line}");
        }
        println!();
    }
    Ok(())
}

pub(crate) fn comment(reference: String, body: String, server: Option<String>) -> Result<()> {
    if body.trim().is_empty() {
        bail!("comment body cannot be empty");
    }
    let (user, slug) = parse_user_slug(&reference)?;
    let c = client(server, true)?;
    let posted = c.post_comment(&user, &slug, &body)?;
    println!("Posted comment {} on @{user}/{slug}.", posted.id);
    Ok(())
}

pub(crate) fn like(reference: String, undo: bool, server: Option<String>) -> Result<()> {
    let (user, slug) = parse_user_slug(&reference)?;
    let c = client(server, true)?;
    let resp = if undo {
        c.unlike(&user, &slug)?
    } else {
        c.like(&user, &slug)?
    };
    println!(
        "@{user}/{slug}: liked={}  total={}",
        resp.liked, resp.like_count
    );
    Ok(())
}

pub(crate) fn notifications(mark_read: bool, server: Option<String>) -> Result<()> {
    let c = client(server, true)?;
    let list = if mark_read {
        c.mark_notifications_read()?
    } else {
        c.notifications()?
    };
    println!("Unread: {}", list.unread);
    if list.items.is_empty() {
        println!("(no notifications)");
        return Ok(());
    }
    for n in &list.items {
        let read = if n.read { " " } else { "•" };
        let target = n
            .target_model
            .as_ref()
            .map(|m| format!("@{}/{}", m.user.handle, m.slug))
            .unwrap_or_else(|| "(unknown)".to_string());
        let source = n
            .source_model
            .as_ref()
            .map(|m| format!("@{}/{}", m.user.handle, m.slug))
            .unwrap_or_default();
        println!(
            "{read} [{}] {}  target={}  source={}  ({})",
            n.kind, n.created_at, target, source, n.id,
        );
    }
    Ok(())
}
