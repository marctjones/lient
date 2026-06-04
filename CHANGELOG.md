# Changelog

All notable changes to Lient. Format: [Keep a Changelog](https://keepachangelog.com/);
SemVer (pre-1.0: minor = features, patch = fixes).

## [Unreleased]

### In progress
- Assign / create issue in the GUI (already in `lient-core` + CLI).
- Interactive OAuth 2.0 (3LO) "Sign in with Atlassian" flow (routing is done;
  the browser + loopback exchange is pending).
- Command palette, notification inbox, local cache / offline.

## [0.1.0] - 2026-06-04

First tagged release — a working personal Jira daily-driver foundation.

### Added
- **`lient-core`** (UI-agnostic): Jira REST client (search, issue, transitions,
  comment, assign, create), typed models, slash-safe URL building.
- **All common auth methods**: Cloud API token (Basic), Server/DC username+password
  (Basic) and Personal Access Token (Bearer), OAuth 2.0 routing
  (api.atlassian.com/ex/jira/{cloudId}), and a raw-header escape hatch.
- **`Jira` trait + `MockJira`** fixtures — the whole app runs offline in demo mode
  and switches to live Jira with a token.
- **`lient-cli`**: a runnable frontend (`whoami`/`list`/`view`/`transitions`/
  `comment`/`open`), with `--demo`.
- **`lient` GUI** (Slint, native): My Work list + issue detail (status, fields,
  description, comments), transition buttons (showing required fields), add-comment,
  open-in-browser; worker-threaded so the UI never blocks on the network.
- **Login wizard**: pick deployment + method, enter credentials, **Test connection**,
  Save. Falls back to demo data when no config is present.

### Notes
- Early release. The Windows binary is built by CI on tag.
- `Cargo.lock` pins `typed-index-collections` to 3.3.0 (a Slint transitive dep
  bumped its MSRV to rustc 1.90); unpin if your toolchain is 1.90+.
