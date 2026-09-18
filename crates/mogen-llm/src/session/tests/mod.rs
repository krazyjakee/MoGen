//! Split by concern to stay under the project's ~1000-line file guideline
//! (this was previously one 1126+ line `tests.rs`). `support` holds fixtures
//! shared by every submodule below.
mod support;

mod control_budget;
mod dependencies;
mod fixtures_http;
mod journal_resume;
mod locks_scope;
mod review_protocol;
mod tools_measure;
