# Changelog

All notable changes to Lient. Format: [Keep a Changelog](https://keepachangelog.com/);
SemVer (pre-1.0: minor = features, patch = fixes).

## [Unreleased]

### In progress
- Interactive OAuth 2.0 (3LO) "Sign in with Atlassian" — needs a registered
  Atlassian OAuth app (client id) + Windows verification.
- Local cache / offline.
- Edit dialog: prefilling current custom-field values.

## [0.1.7] - 2026-06-04

### Added
- **Arrow-key palette navigation** — Up/Down move the highlight, Enter runs the
  highlighted command (click still works).
- **Dynamic required fields on Create** — the New-issue dialog now renders the
  selected project/type's required fields (e.g. a required custom select), and
  rebuilds them instantly when you change the project/type. Createmeta is fetched
  with fields and cached.
- **Inbox view** — a *My Work / Inbox* toggle; Inbox shows issues assigned to you
  updated in the last 7 days (recent activity / needs-attention), freshest first.

## [0.1.6] - 2026-06-04

### Added
- **Command palette** — `⌘ Commands` (or **Ctrl-K**) opens a searchable palette:
  type to filter (New issue / Refresh / Edit / Open in browser / Account / Quit),
  Enter runs the top match, click to run, **Esc** to dismiss. Filtering is done
  in Rust (Slint strings can't substring-match); commands re-invoke the existing
  actions. A root `FocusScope` provides the Ctrl-K / Esc shortcuts.

## [0.1.5] - 2026-06-04

### Added
- **Create issue** — a **＋ New** button opens a dialog: pick Project / Type
  (from `createmeta`), enter Summary + Description, Create. The new issue is
  selected on success. Core adds `create_targets`.
- **Labels in the Edit dialog** — comma-separated, prefilled from the issue's
  current labels; submitted as a string array.

## [0.1.4] - 2026-06-04

### Added
- **Reply to requestor (Jira Service Management)** — a comment-visibility toggle:
  *Internal note* (default) vs *Reply to customer* (public). Public replies go
  through the JSM `servicedeskapi` (`request/{key}/comment` with `public: true`);
  internal stays on the core comment API. Core adds `add_request_comment`.

## [0.1.3] - 2026-06-04

### Added
- **Assignee in the Edit dialog** — a dropdown of assignable users (from
  `/user/assignable/search`), prefilled with the current assignee; saving routes
  through the dedicated `/assignee` endpoint (correct Cloud `accountId` / Server
  `name` shape). Core adds `assignable_users`.

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
