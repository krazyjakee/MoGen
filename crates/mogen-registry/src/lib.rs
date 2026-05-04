//! Cross-author module registry semantics for `.mog` files.
//!
//! This crate is the single source of truth for everything that has to agree
//! between MoGHub (the registry server), the desktop CLI, and MoGen Studio
//! whenever a `.mog` file references another author's module via
//! `use "@user/slug[@version]"`. Splitting it out of MoGHub's
//! `src/mogen.rs` lets `mogen build` and Studio resolve registry refs
//! locally without dragging in axum/sqlx, while the server keeps using the
//! same parser to stamp `model_versions.mog_lock` at upload time.
//!
//! Layers:
//! - [`refs`] — `@user/slug[@v]` parsing + `extract_use_graph` AST walk.
//! - [`lockfile`] — `mog.lock` reader/writer. Format extends MoGHub's
//!   `mog_lock` JSON with a desktop-only `"resolved"` table; the server
//!   ignores unknown keys so a Studio-written lock survives upload.
//! - [`cache`] — on-disk layout for fetched registry files at
//!   `$MOGEN_CACHE_DIR/registry/<user>/<slug>/<version>/...`. Versions
//!   are immutable `i32`s server-side, so cache is poisoning-safe by
//!   construction.
//! - [`client`] — `RegistryClient` trait that the desktop HTTP client and
//!   the in-process server resolver both implement. Sync, so Studio's
//!   `Loader` can satisfy `mogen-dsl` without spinning up a runtime.

pub mod cache;
pub mod client;
pub mod lockfile;
pub mod loader;
pub mod refs;

pub use cache::{cache_root, registry_dir, version_dir};
pub use client::{FetchedFile, FetchedVersion, RegistryClient};
pub use loader::RegistryLoader;
pub use lockfile::{LockFile, LockResolved, LockResolvedFile};
pub use refs::{extract_use_graph, parse_registry_ref, RegistryRef, UseGraph};
