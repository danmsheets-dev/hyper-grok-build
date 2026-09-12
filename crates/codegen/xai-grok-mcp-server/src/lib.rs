//! Serves Turbo's tool registry over MCP to external clients.
//!
//! The intended consumer is a ChatGPT Developer-mode custom app reaching a
//! loopback server through a tunnel, so that a ChatGPT subscription can drive
//! Turbo's tools. Turbo remains the workbench; the external client is the loop.
//!
//! # Read [`guard`] before changing anything here
//!
//! Exposing this toolset to a third party is not the same as running it locally
//! for an operator who already trusts the machine. The permission machinery that
//! protects an interactive session (`CompiledPolicy`,
//! `confine_access_outside_root`, `edit_target_protection`) is driven from ACP
//! session setup and does not run on the `FinalizedToolset::call` path this
//! crate uses. [`guard::PathGuard`] is therefore the entire boundary, and it is
//! written to fail closed in every direction.
//!
//! # What is deliberately not served
//!
//! `run_terminal_cmd` and every other shell surface. A shell command's operands
//! cannot be enumerated, and the bash tool performs no containment of its own,
//! so serving it would mean handing an external party an unbounded shell on the
//! operator's machine. On platforms where `xai-grok-sandbox` is a no-op —
//! Windows included — nothing else would catch it.

pub mod annotate;
pub mod guard;
pub mod handler;
pub mod http;
pub mod oauth;
mod private_dir;
pub mod read_confined_fs;
pub mod toolset;
pub mod tunnel;

#[cfg(test)]
mod guard_tests;
#[cfg(test)]
mod handler_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod toolset_tests;

pub use guard::{Access, Denial, PathGuard, Reason, admit, log_preview, validate_tool_schema};
pub use private_dir::remove_session_dirs_for_abort;
