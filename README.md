<p align="center">
  <img src="assets/logo.svg" alt="Lient logo" width="120" height="120">
</p>

# Lient

A fast, native **personal Jira daily-driver** — for getting through your assigned
work (see, transition, comment, create) without the slow web UI. Deliberately
*not* a Jira admin tool: configuration is left to the web; this is the pleasant
all-day client. Built in Rust (same stack as [Noet](https://github.com/marctjones/noet)).

> The name is a misspelling of *client* — a tiny typo-named tool, like Noet (*note*).

> **Status: engine + GUI working (v0.1).** The native Slint app runs with a login
> wizard, My Work list, issue detail, comment, and status transitions. OAuth's
> interactive flow and a few GUI actions are still in progress — see [CHANGELOG](CHANGELOG.md).

## Architecture

```
crates/
  core/  →  lient-core (lib)  Jira REST client, models, config, auth, mock — NO ui deps
  cli/   →  lient-cli   (bin)  a scriptable frontend over the core
  gui/   →  lient        (bin)  the native Slint desktop app
```

## Run the GUI

```bash
cargo run -p lient-gui            # opens the login wizard (or your saved account)
cargo run -p lient-gui -- --demo  # explore with fixture data, no Jira needed
```

## Try it now — demo mode (no Jira needed)

Runs against in-memory fixtures, so you can see it work with zero setup:

```bash
cargo run -p lient-cli -- --demo list
cargo run -p lient-cli -- --demo view ACME-101
cargo run -p lient-cli -- --demo transitions ACME-101
```

## Connect to your Jira

Set env vars (or write `~/.config/lient/config.json`). Tokens are **local only**,
never committed.

```bash
# Jira Cloud (email + API token):
export LIENT_URL=https://yourorg.atlassian.net
export LIENT_EMAIL=you@company.com
export LIENT_TOKEN=<api-token>      # id.atlassian.com → API tokens

# Jira Server / Data Center (Personal Access Token):
export LIENT_URL=https://jira.company.com
export LIENT_TOKEN=<personal-access-token>   # (no LIENT_EMAIL → Bearer/PAT)

cargo run -p lient-cli -- whoami            # confirm the connection
cargo run -p lient-cli -- list             # your open issues
cargo run -p lient-cli -- view ENG-12      # one issue + comments
cargo run -p lient-cli -- transitions ENG-12
cargo run -p lient-cli -- comment ENG-12 "on it"
cargo run -p lient-cli -- open ENG-12      # open in browser
```

## Supported login methods

`lient-core` implements every common way to authenticate:

| Method | Flavor | How |
|---|---|---|
| API token | Cloud | email + token (Basic) |
| Username + password | Server/DC | user + pass (Basic) |
| Personal Access Token | Server/DC | PAT (Bearer) |
| OAuth 2.0 (3LO) | Cloud | "Sign in with Atlassian" — routing implemented; interactive flow lands with the GUI |
| Raw header | any (SSO/proxy) | paste an `Authorization` header |

## Develop

```bash
cargo test            # 10 core tests (auth, parsing, mock read/write) — no server needed
```

The `Jira` trait (`lient_core::Jira`) is the seam: `JiraClient` hits the network,
`MockJira` serves fixtures. The GUI talks to `Box<dyn Jira>`, so the entire app
runs in demo mode offline and switches to live Jira by providing a token.

## License

TBD.
