//! Lient CLI — a thin, runnable frontend over `lient-core` so the whole pipeline
//! (auth → REST → parse) can be exercised against a real Jira before the Slint
//! GUI exists.
//!
//! Configure once, then:
//!   LIENT_URL=https://you.atlassian.net LIENT_EMAIL=you@co.com LIENT_TOKEN=xxx \
//!     cargo run -p lient-cli -- whoami
//!
//!   lient whoami                 # confirm the connection
//!   lient list                   # your open issues
//!   lient view ENG-12            # one issue + its comments
//!   lient open ENG-12            # open it in the browser
//!   lient transitions ENG-12     # available status moves
//!   lient comment ENG-12 "text"  # add a comment

use anyhow::{bail, Result};
use lient_core::{Jira, JiraClient, JiraConfig, MockJira};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // `--demo` (or LIENT_DEMO=1) runs against in-memory fixtures — no server.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let demo = std::env::var("LIENT_DEMO").is_ok() || args.iter().any(|a| a == "--demo");
    args.retain(|a| a != "--demo");
    let cmd = args.first().map(String::as_str).unwrap_or("list");

    let client: Box<dyn Jira> = if demo {
        Box::new(MockJira::new())
    } else {
        Box::new(JiraClient::new(JiraConfig::load()?))
    };

    match cmd {
        "whoami" => {
            let me = client.myself()?;
            let id = if me.account_id.is_empty() { &me.name } else { &me.account_id };
            println!("Connected as {} <{}>  [{}]", me.display_name, me.email, id);
        }
        "list" => {
            let issues = client.my_issues(50)?;
            if issues.is_empty() {
                println!("No open issues assigned to you. 🎉");
            }
            for i in &issues {
                println!(
                    "{:<10} {:<13} {:<8} {}",
                    i.key,
                    truncate(i.status(), 13),
                    truncate(i.priority(), 8),
                    i.summary()
                );
            }
        }
        "view" => {
            let key = arg(&args, 1, "view <KEY>")?;
            let i = client.issue(key)?;
            println!("{}  [{}]  {}", i.key, i.status(), i.summary());
            println!("type: {}   priority: {}   assignee: {}", i.issue_type(), i.priority(), i.assignee());
            if let Some(d) = &i.fields.description {
                println!("\n{}\n", d.trim());
            }
            if let Some(c) = &i.fields.comment {
                println!("--- {} comment(s) ---", c.total);
                for cm in &c.comments {
                    let who = cm.author.as_ref().map(|a| a.display_name.as_str()).unwrap_or("?");
                    println!("[{}] {}: {}", cm.created, who, cm.body.trim());
                }
            }
        }
        "transitions" => {
            let key = arg(&args, 1, "transitions <KEY>")?;
            for t in client.transitions(key)? {
                let to = t.to.as_ref().map(|n| n.name.as_str()).unwrap_or("?");
                let req: Vec<&str> = t.fields.iter().filter(|(_, f)| f.required).map(|(k, _)| k.as_str()).collect();
                let reqs = if req.is_empty() { String::new() } else { format!("  (requires: {})", req.join(", ")) };
                println!("{:<4} {} → {}{}", t.id, t.name, to, reqs);
            }
        }
        "comment" => {
            let key = arg(&args, 1, "comment <KEY> <text>")?;
            let body = args.get(2..).map(|s| s.join(" ")).unwrap_or_default();
            if body.is_empty() {
                bail!("comment <KEY> <text> — text required");
            }
            client.add_comment(key, &body)?;
            println!("commented on {key}");
        }
        "open" => {
            let key = arg(&args, 1, "open <KEY>")?;
            let url = client.browse_url(key);
            open::that(&url)?;
            println!("opened {url}");
        }
        other => bail!("unknown command '{other}' — try: whoami | list | view | transitions | comment | open"),
    }
    Ok(())
}

fn arg<'a>(args: &'a [String], i: usize, usage: &str) -> Result<&'a str> {
    args.get(i).map(String::as_str).ok_or_else(|| anyhow::anyhow!("usage: lient {usage}"))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}
