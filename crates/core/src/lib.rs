//! Lient core — a UI-agnostic Jira REST client for a fast personal desktop app.
//!
//! Scope is deliberately small (a daily-driver, not an admin tool): search and
//! read issues, transition status (honoring required screen fields), comment,
//! assign, create. No GUI dependencies, so the Slint frontend (`lient`) — or a
//! future TUI, or tests — all build on the same engine.

pub mod api;
pub mod client;
pub mod config;
pub mod mock;
pub mod model;

pub use api::Jira;
pub use client::JiraClient;
pub use config::{Auth, JiraConfig};
pub use mock::MockJira;
pub use model::{Comment, Issue, Transition, User};
