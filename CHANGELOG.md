# Changelog

All notable changes to Lient. Format: [Keep a Changelog](https://keepachangelog.com/);
SemVer (pre-1.0: minor = features, patch = fixes).

## [Unreleased]

### In progress
- Assign / create issue in the GUI (already in `lient-core` + CLI).
- Interactive OAuth 2.0 (3LO) "Sign in with Atlassian" flow (routing is done;
  the browser + loopback exchange is pending).
- Command palette, notification inbox, local cache / offline.
- Edit dialog: labels/assignee fields + prefilling current custom-field values.

## [0.1.2] - 2026-06-04

### Added
- **Edit issue fields** — an **✎ Edit** button on the detail pane opens a dialog
  built from `/issue/{key}/editmeta`: dropdowns for pick-lists (priority and
  **custom fields** with options), text boxes otherwise, prefilled from current
  values. Submits only the fields you changed via `PUT /issue`. Core adds
  `edit_meta` + `update_issue` on the client, trait, and mock.

## [0.1.1] - 2026-06-04

### Added
- **Transition required-field dialog** — transitions that need fields (e.g.
  *Done* → *Resolution*) now open a dialog that renders each required field as a
  dropdown (for pick-lists, from `allowedValues`) or a text box, and submits the
  correct JSON. Core captures the field schema + allowed values.
- **Menu bar** (File / Issue / Help) — in-window overlay menus with instant open
  and hover-to-switch; Refresh, Account, Quit, Open-in-browser, and help links.

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
