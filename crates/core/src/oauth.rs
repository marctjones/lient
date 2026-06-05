//! OAuth 2.0 (3LO) "Sign in with Atlassian" for Jira Cloud — PKCE + a loopback
//! redirect, no client secret needed for a public desktop client.
//!
//! Requires a registered Atlassian OAuth 2.0 (3LO) app (you create it once at
//! developer.atlassian.com and pass its **client id**). Add the redirect URI
//! `http://localhost/callback` (Atlassian allows any loopback port at runtime).
//!
//! What's unit-tested here (no network): PKCE pair shape, the authorize-URL
//! builder, and parsing the loopback redirect. The token exchange and
//! accessible-resources calls hit Atlassian and are verified live.

use crate::config::{Auth, JiraConfig};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{SystemTime, UNIX_EPOCH};

const AUTHORIZE: &str = "https://auth.atlassian.com/authorize";
const TOKEN: &str = "https://auth.atlassian.com/oauth/token";
const RESOURCES: &str = "https://api.atlassian.com/oauth/token/accessible-resources";
const SCOPES: &str = "read:jira-work write:jira-work read:jira-user offline_access";

/// Run the full interactive login and return a ready [`JiraConfig`] (OAuth auth).
/// Blocks until the user authorizes in the browser, so call it off the UI thread.
pub fn login(client_id: &str, client_secret: Option<&str>) -> Result<JiraConfig> {
    if client_id.trim().is_empty() {
        bail!("OAuth client id is required (register an app at developer.atlassian.com)");
    }
    let (verifier, challenge) = pkce_pair();
    let state = rand_token(8);
    let listener = TcpListener::bind("127.0.0.1:0").context("binding loopback for OAuth redirect")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}/callback");

    let url = authorize_url(client_id, &redirect_uri, &state, &challenge);
    open::that(&url).context("opening the browser for Atlassian sign-in")?;

    let code = await_code(&listener, &state)?;
    let token = exchange_code(client_id, client_secret, &code, &redirect_uri, &verifier)?;
    let (cloud_id, site) = first_resource(&token.access_token)?;

    Ok(JiraConfig {
        base_url: site,
        auth: Auth::OAuth {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at: now_secs() + token.expires_in,
            cloud_id,
            client_id: client_id.to_string(),
        },
        api_version: "3".into(),
    })
}

/// Exchange a refresh token for a fresh access token. Returns
/// (access_token, refresh_token, expires_at_epoch_secs).
pub fn refresh(client_id: &str, refresh_token: &str) -> Result<(String, String, i64)> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "client_id": client_id,
        "refresh_token": refresh_token,
    });
    let resp = ureq::post(TOKEN).set("Content-Type", "application/json").send_string(&body.to_string());
    let text = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(c, r)) => bail!("token refresh failed ({c}): {}", r.into_string().unwrap_or_default()),
        Err(e) => bail!("token refresh request failed: {e}"),
    };
    let t: TokenResponse = serde_json::from_str(&text).context("parsing the refresh response")?;
    // Atlassian may or may not rotate the refresh token; keep the old if absent.
    let new_refresh = if t.refresh_token.is_empty() { refresh_token.to_string() } else { t.refresh_token };
    Ok((t.access_token, new_refresh, now_secs() + t.expires_in))
}

/// Seconds since the Unix epoch (exposed for the client's expiry check).
pub fn epoch_secs() -> i64 {
    now_secs()
}

/// (code_verifier, code_challenge) for PKCE S256.
pub fn pkce_pair() -> (String, String) {
    let verifier = rand_token(32);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The Atlassian authorize URL the browser is sent to.
pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    format!(
        "{AUTHORIZE}?audience=api.atlassian.com&client_id={cid}&scope={scope}&redirect_uri={ru}&state={state}&response_type=code&prompt=consent&code_challenge={ch}&code_challenge_method=S256",
        cid = enc(client_id),
        scope = enc(SCOPES),
        ru = enc(redirect_uri),
        state = enc(state),
        ch = enc(challenge),
    )
}

/// Parse `code` and `state` from a loopback request's first line.
pub fn parse_callback(req: &str) -> Result<(String, String)> {
    let line = req.lines().next().unwrap_or("");
    let path = line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split('?').nth(1).unwrap_or("");
    let (mut code, mut state) = (String::new(), String::new());
    for kv in query.split('&') {
        let mut it = kv.splitn(2, '=');
        match (it.next(), it.next()) {
            (Some("code"), Some(v)) => code = dec(v),
            (Some("state"), Some(v)) => state = dec(v),
            (Some("error"), Some(v)) => bail!("Atlassian returned an error: {}", dec(v)),
            _ => {}
        }
    }
    if code.is_empty() {
        bail!("no authorization code in the redirect");
    }
    Ok((code, state))
}

fn await_code(listener: &TcpListener, expect_state: &str) -> Result<String> {
    let (mut stream, _) = listener.accept().context("waiting for the OAuth redirect")?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let parsed = parse_callback(&req);
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body style='font-family:sans-serif;text-align:center;padding-top:60px'><h2>Lient is signed in \xe2\x9c\x93</h2><p>You can close this tab and return to Lient.</p></body></html>",
    );
    let (code, state) = parsed?;
    if state != expect_state {
        bail!("OAuth state mismatch (possible CSRF) — aborting");
    }
    Ok(code)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: i64,
}

fn exchange_code(client_id: &str, secret: Option<&str>, code: &str, redirect_uri: &str, verifier: &str) -> Result<TokenResponse> {
    let mut body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id,
        "code": code,
        "redirect_uri": redirect_uri,
        "code_verifier": verifier,
    });
    if let Some(s) = secret {
        body["client_secret"] = serde_json::json!(s);
    }
    let resp = ureq::post(TOKEN)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string());
    let text = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(c, r)) => bail!("token exchange failed ({c}): {}", r.into_string().unwrap_or_default()),
        Err(e) => bail!("token exchange request failed: {e}"),
    };
    serde_json::from_str(&text).context("parsing the OAuth token response")
}

#[derive(Deserialize)]
struct Resource {
    id: String,
    #[serde(default)]
    url: String,
}

/// The first accessible Jira site → (cloudId, siteUrl).
fn first_resource(access_token: &str) -> Result<(String, String)> {
    let resp = ureq::get(RESOURCES)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("Accept", "application/json")
        .call();
    let text = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(c, r)) => bail!("accessible-resources failed ({c}): {}", r.into_string().unwrap_or_default()),
        Err(e) => bail!("accessible-resources request failed: {e}"),
    };
    let list: Vec<Resource> = serde_json::from_str(&text).context("parsing accessible-resources")?;
    let r = list.into_iter().next().context("your Atlassian account has no accessible Jira sites")?;
    Ok((r.id, r.url))
}

// ---- small helpers (no deps) ----------------------------------------------

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// URL-safe base64 without padding (for PKCE).
fn base64url(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(T[(b2 & 0x3f) as usize] as char);
        }
    }
    out
}

/// A random URL-safe token of `bytes` of entropy.
fn rand_token(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    getrandom::getrandom(&mut raw).expect("OS RNG");
    base64url(&raw)
}

/// Percent-encode a query value (encode everything not unreserved).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-decode (+ as space) a query value.
fn dec(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_is_well_formed() {
        let (v, c) = pkce_pair();
        // verifier: 32 bytes → 43 url-safe base64 chars, no padding
        assert_eq!(v.len(), 43);
        assert!(!v.contains('=') && !v.contains('+') && !v.contains('/'));
        // challenge: sha256 (32 bytes) → 43 chars
        assert_eq!(c.len(), 43);
        // S256 known vector: challenge = base64url(sha256(verifier))
        assert_eq!(c, base64url(&Sha256::digest(v.as_bytes())));
    }

    #[test]
    fn base64url_known_vectors() {
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        // padding stripped, url-safe alphabet
        assert_eq!(base64url(b"fo"), "Zm8");
    }

    #[test]
    fn authorize_url_has_required_params() {
        let u = authorize_url("CID", "http://localhost:5000/callback", "st8", "chal");
        assert!(u.starts_with(AUTHORIZE));
        assert!(u.contains("client_id=CID"));
        assert!(u.contains("code_challenge=chal"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains("response_type=code"));
        // redirect_uri is percent-encoded
        assert!(u.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback"));
    }

    #[test]
    fn parse_callback_extracts_code_and_state() {
        let req = "GET /callback?code=ABC123&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let (code, state) = parse_callback(req).unwrap();
        assert_eq!(code, "ABC123");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn parse_callback_surfaces_errors_and_decodes() {
        assert!(parse_callback("GET /callback?error=access_denied HTTP/1.1\r\n").is_err());
        let (code, _) = parse_callback("GET /callback?code=a%2Bb&state=s HTTP/1.1\r\n").unwrap();
        assert_eq!(code, "a+b"); // percent-decoded
    }
}
