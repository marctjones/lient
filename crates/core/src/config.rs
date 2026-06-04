//! Connection config + the personal token. Stored locally in the OS config dir
//! (`%APPDATA%\lient` / `~/.config/lient`), NEVER in a repo. Env vars override,
//! which is handy for first-run / CI without writing a file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Every supported way to authenticate to Jira.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    /// Jira Cloud API token: `email` + token, sent as `Basic base64(email:token)`.
    Basic { email: String, token: String },
    /// Jira Server/DC username + password, sent as `Basic base64(user:pass)`.
    Password { username: String, password: String },
    /// Jira Server/DC Personal Access Token, sent as `Bearer <token>`.
    Bearer { token: String },
    /// Jira Cloud OAuth 2.0 (3LO). Calls route via api.atlassian.com/ex/jira/{cloud_id}.
    OAuth {
        access_token: String,
        #[serde(default)]
        refresh_token: String,
        /// Unix epoch seconds when the access token expires (for refresh).
        #[serde(default)]
        expires_at: i64,
        /// The Atlassian cloud id of the selected site (from accessible-resources).
        cloud_id: String,
    },
    /// Escape hatch for SSO/proxy instances: a verbatim `Authorization` header.
    Raw { header: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraConfig {
    /// e.g. `https://yourorg.atlassian.net` or `https://jira.company.com`.
    pub base_url: String,
    pub auth: Auth,
    /// REST API version — "2" by default (plain-string bodies, works on Cloud + Server).
    #[serde(default = "default_api")]
    pub api_version: String,
}

fn default_api() -> String {
    "2".into()
}

impl JiraConfig {
    /// `<config_dir>/lient/config.json`
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lient")
            .join("config.json")
    }

    /// Load config: env vars win, else the on-disk file.
    ///
    /// Env: `LIENT_URL`, and either `LIENT_EMAIL`+`LIENT_TOKEN` (Cloud) or just
    /// `LIENT_TOKEN` (Server PAT).
    pub fn load() -> Result<JiraConfig> {
        if let Ok(base_url) = std::env::var("LIENT_URL") {
            let token = std::env::var("LIENT_TOKEN")
                .context("LIENT_URL set but LIENT_TOKEN missing")?;
            let auth = match std::env::var("LIENT_EMAIL") {
                Ok(email) if !email.is_empty() => Auth::Basic { email, token },
                _ => Auth::Bearer { token },
            };
            return Ok(JiraConfig { base_url, auth, api_version: default_api() });
        }
        let path = Self::config_path();
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("no Lient config — set LIENT_* env vars or write {}", path.display()))?;
        serde_json::from_str(&raw).context("parsing Lient config.json")
    }

    /// Persist config to the local config dir (0600-ish — it holds a token).
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The `Authorization` header value for this config.
    pub fn auth_header(&self) -> String {
        match &self.auth {
            Auth::Basic { email, token } => {
                format!("Basic {}", base64(format!("{email}:{token}").as_bytes()))
            }
            Auth::Password { username, password } => {
                format!("Basic {}", base64(format!("{username}:{password}").as_bytes()))
            }
            Auth::Bearer { token } => format!("Bearer {token}"),
            Auth::OAuth { access_token, .. } => format!("Bearer {access_token}"),
            Auth::Raw { header } => header.clone(),
        }
    }

    /// REST base URL. OAuth (Cloud 3LO) routes through api.atlassian.com keyed by
    /// the site's cloud id; everything else hits the site directly.
    fn rest_base(&self) -> String {
        match &self.auth {
            Auth::OAuth { cloud_id, .. } => {
                format!("https://api.atlassian.com/ex/jira/{cloud_id}")
            }
            _ => self.base_url.trim_end_matches('/').to_string(),
        }
    }

    /// `<rest_base>/rest/api/<ver>/<path>` with redundant slashes avoided.
    pub fn api_url(&self, path: &str) -> String {
        format!(
            "{}/rest/api/{}/{}",
            self.rest_base(),
            self.api_version,
            path.trim_start_matches('/')
        )
    }

    /// `<rest_base>/rest/servicedeskapi/<path>` — Jira Service Management API
    /// (public/internal customer replies live here, not under /rest/api).
    pub fn servicedesk_url(&self, path: &str) -> String {
        format!("{}/rest/servicedeskapi/{}", self.rest_base(), path.trim_start_matches('/'))
    }

    /// `<base>/browse/<KEY>` — for "open in browser".
    pub fn browse_url(&self, key: &str) -> String {
        format!("{}/browse/{}", self.base_url.trim_end_matches('/'), key)
    }
}

/// Minimal standard base64 (no padding tricks) — avoids a dependency for the one
/// place we need it (HTTP Basic auth).
fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 { T[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(b2 & 0x3f) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // the real shape we send: email:token
        assert_eq!(base64(b"a@b.com:tok"), "YUBiLmNvbTp0b2s=");
    }

    #[test]
    fn auth_headers() {
        let cloud = JiraConfig {
            base_url: "https://x.atlassian.net".into(),
            auth: Auth::Basic { email: "a@b.com".into(), token: "tok".into() },
            api_version: "2".into(),
        };
        assert_eq!(cloud.auth_header(), "Basic YUBiLmNvbTp0b2s=");

        let server = JiraConfig {
            base_url: "https://jira.co/".into(),
            auth: Auth::Bearer { token: "PAT123".into() },
            api_version: "2".into(),
        };
        assert_eq!(server.auth_header(), "Bearer PAT123");
    }

    #[test]
    fn url_building_is_slash_safe() {
        let c = JiraConfig {
            base_url: "https://jira.co/".into(),
            auth: Auth::Bearer { token: "x".into() },
            api_version: "2".into(),
        };
        assert_eq!(c.api_url("search"), "https://jira.co/rest/api/2/search");
        assert_eq!(c.api_url("/issue/ENG-1"), "https://jira.co/rest/api/2/issue/ENG-1");
        assert_eq!(c.browse_url("ENG-1"), "https://jira.co/browse/ENG-1");
    }

    #[test]
    fn all_auth_methods_produce_headers() {
        let mk = |auth| JiraConfig { base_url: "https://x.atlassian.net".into(), auth, api_version: "3".into() };
        assert_eq!(
            mk(Auth::Password { username: "marc".into(), password: "pw".into() }).auth_header(),
            format!("Basic {}", base64(b"marc:pw"))
        );
        assert_eq!(mk(Auth::Bearer { token: "PAT".into() }).auth_header(), "Bearer PAT");
        assert_eq!(
            mk(Auth::OAuth { access_token: "AT".into(), refresh_token: "RT".into(), expires_at: 0, cloud_id: "cid".into() }).auth_header(),
            "Bearer AT"
        );
        assert_eq!(
            mk(Auth::Raw { header: "Bearer custom".into() }).auth_header(),
            "Bearer custom"
        );
    }

    #[test]
    fn oauth_routes_through_atlassian_api() {
        let c = JiraConfig {
            base_url: "https://acme.atlassian.net".into(),
            auth: Auth::OAuth { access_token: "AT".into(), refresh_token: String::new(), expires_at: 0, cloud_id: "abc-123".into() },
            api_version: "3".into(),
        };
        // API calls go to api.atlassian.com/ex/jira/{cloudId}, …
        assert_eq!(c.api_url("myself"), "https://api.atlassian.com/ex/jira/abc-123/rest/api/3/myself");
        // …but the browse link still points at the human site.
        assert_eq!(c.browse_url("ENG-1"), "https://acme.atlassian.net/browse/ENG-1");
    }

    #[test]
    fn config_roundtrips_through_json() {
        let c = JiraConfig {
            base_url: "https://x.atlassian.net".into(),
            auth: Auth::Basic { email: "a@b.com".into(), token: "t".into() },
            api_version: "2".into(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: JiraConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.auth, c.auth);
        assert_eq!(back.base_url, c.base_url);
    }
}
