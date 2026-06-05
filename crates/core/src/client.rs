//! The Jira REST client. Blocking (ureq) — fine for a desktop app; each call is
//! a quick request and the GUI runs them off the UI thread. Read + light-write
//! only (the Lient scope): search, fetch, transition, comment, assign, create.

use crate::config::{Auth, JiraConfig};
use crate::model::*;
use anyhow::{bail, Result};
use serde_json::json;
use std::sync::Mutex;

pub struct JiraClient {
    /// Behind a Mutex so an expired OAuth token can be refreshed in place before
    /// a request (and re-persisted), transparently to callers.
    cfg: Mutex<JiraConfig>,
    agent: ureq::Agent,
}

/// Default fields we ask Jira to return for list/detail (keeps payloads small).
const LIST_FIELDS: &str = "summary,status,assignee,priority,issuetype,labels,updated,duedate";
const DETAIL_FIELDS: &str = "summary,status,assignee,reporter,priority,issuetype,labels,updated,created,duedate,description,comment";

impl JiraClient {
    pub fn new(cfg: JiraConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(20))
            .build();
        Self { cfg: Mutex::new(cfg), agent }
    }

    // ---- config access (locks) ------------------------------------------
    fn api_url(&self, path: &str) -> String {
        self.cfg.lock().unwrap().api_url(path)
    }
    fn servicedesk_url(&self, path: &str) -> String {
        self.cfg.lock().unwrap().servicedesk_url(path)
    }
    pub fn browse_url(&self, key: &str) -> String {
        self.cfg.lock().unwrap().browse_url(key)
    }
    /// Cloud (API token / OAuth) identifies users by accountId; Server/DC by name.
    fn is_cloud(&self) -> bool {
        matches!(self.cfg.lock().unwrap().auth, Auth::Basic { .. } | Auth::OAuth { .. })
    }

    /// Refresh the OAuth access token if it's expired (or within 60s of it).
    /// No-op for non-OAuth auth. Network call is made without holding the lock.
    fn ensure_fresh(&self) {
        let params = {
            let cfg = self.cfg.lock().unwrap();
            match &cfg.auth {
                Auth::OAuth { expires_at, refresh_token, client_id, .. }
                    if *expires_at != 0 && !refresh_token.is_empty() && crate::oauth::epoch_secs() >= *expires_at - 60 =>
                {
                    Some((client_id.clone(), refresh_token.clone()))
                }
                _ => None,
            }
        };
        let Some((client_id, refresh_token)) = params else { return };
        if let Ok((access, new_refresh, new_exp)) = crate::oauth::refresh(&client_id, &refresh_token) {
            let mut cfg = self.cfg.lock().unwrap();
            if let Auth::OAuth { access_token, refresh_token, expires_at, .. } = &mut cfg.auth {
                *access_token = access;
                *refresh_token = new_refresh;
                *expires_at = new_exp;
            }
            let _ = cfg.save();
        }
    }

    fn auth_header(&self) -> String {
        self.cfg.lock().unwrap().auth_header()
    }

    // ---- reads -----------------------------------------------------------

    /// Issues assigned to the authenticated user, most-recently-updated first.
    pub fn my_issues(&self, max: u32) -> Result<Vec<Issue>> {
        Ok(self.search(&my_issues_jql(), max)?.issues)
    }

    /// Arbitrary JQL search.
    pub fn search(&self, jql: &str, max: u32) -> Result<SearchResult> {
        let url = self.api_url("search");
        let body = self
            .get(&url)
            .query("jql", jql)
            .query("maxResults", &max.to_string())
            .query("fields", LIST_FIELDS)
            .call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Full detail for one issue (description + comments included).
    pub fn issue(&self, key: &str) -> Result<Issue> {
        let url = self.api_url(&format!("issue/{key}"));
        let body = self.get(&url).query("fields", DETAIL_FIELDS).call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Confirm the connection works and return the current user.
    pub fn myself(&self) -> Result<User> {
        let url = self.api_url("myself");
        let body = self.get(&url).call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Available workflow transitions, with the fields each requires.
    pub fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = self.api_url(&format!("issue/{key}/transitions"));
        let body = self.get(&url).query("expand", "transitions.fields").call_text()?;
        let resp: TransitionsResponse = serde_json::from_str(&body)?;
        Ok(resp.transitions)
    }

    /// Projects × issue types you can create in, each with its required fields
    /// (for the "New issue" dialog). One createmeta call, fields included.
    pub fn create_targets(&self) -> Result<Vec<crate::model::CreateOption>> {
        let url = self.api_url("issue/createmeta");
        let body = self.get(&url).query("expand", "projects.issuetypes.fields").call_text()?;
        let meta: crate::model::CreateMeta = serde_json::from_str(&body)?;
        let mut out = Vec::new();
        for p in meta.projects {
            for t in p.issuetypes {
                let required: Vec<(String, crate::model::TransitionField)> = t
                    .fields
                    .into_iter()
                    .filter(|(k, f)| f.required && !matches!(k.as_str(), "summary" | "project" | "issuetype" | "description" | "reporter"))
                    .collect();
                out.push(crate::model::CreateOption {
                    project_key: p.key.clone(),
                    project_name: p.name.clone(),
                    type_name: t.name,
                    required,
                });
            }
        }
        Ok(out)
    }

    /// The issue's raw `fields` object (all fields, including custom) — used to
    /// prefill the edit dialog with current values.
    pub fn raw_fields(&self, key: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
        let url = self.api_url(&format!("issue/{key}"));
        let body = self.get(&url).query("fields", "*all").call_text()?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        Ok(v.get("fields").and_then(|f| f.as_object()).cloned().unwrap_or_default())
    }

    /// Users who can be assigned this issue (for the assignee picker).
    pub fn assignable_users(&self, issue_key: &str) -> Result<Vec<User>> {
        let url = self.api_url("user/assignable/search");
        let body = self.get(&url).query("issueKey", issue_key).query("maxResults", "50").call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Fields editable on this issue (standard + custom), with their types and
    /// allowed values — used to render the edit dialog.
    pub fn edit_meta(&self, key: &str) -> Result<Vec<(String, crate::model::TransitionField)>> {
        let url = self.api_url(&format!("issue/{key}/editmeta"));
        let body = self.get(&url).call_text()?;
        let resp: EditMeta = serde_json::from_str(&body)?;
        Ok(resp.fields.into_iter().collect())
    }

    // ---- light writes ----------------------------------------------------

    /// Update fields on an issue. `fields` is the Jira `fields` object, e.g.
    /// `{"priority":{"id":"2"}, "customfield_10010":{"value":"High"}}`.
    pub fn update_issue(&self, key: &str, fields: serde_json::Value) -> Result<()> {
        let url = self.api_url(&format!("issue/{key}"));
        self.put(&url).send_text(&json!({ "fields": fields }).to_string())?; // 204 on success
        Ok(())
    }

    /// Move an issue through a transition. `extra_fields` carries any required
    /// screen fields (e.g. `{"resolution":{"name":"Done"}}`), rendered by the UI.
    pub fn transition(&self, key: &str, transition_id: &str, extra_fields: serde_json::Value) -> Result<()> {
        let url = self.api_url(&format!("issue/{key}/transitions"));
        let mut payload = json!({ "transition": { "id": transition_id } });
        if let Some(obj) = extra_fields.as_object() {
            if !obj.is_empty() {
                payload["fields"] = extra_fields;
            }
        }
        self.post(&url).send_text(&payload.to_string())?; // 204 No Content on success
        Ok(())
    }

    /// Add a comment on a Jira Service Management request. `public = true` is a
    /// reply visible to the customer/requestor; `false` is an internal agent note.
    /// JSM-only (errors on non-service-desk issues).
    pub fn add_request_comment(&self, key: &str, body: &str, public: bool) -> Result<()> {
        let url = self.servicedesk_url(&format!("request/{key}/comment"));
        self.post(&url).send_text(&json!({ "body": body, "public": public }).to_string())?;
        Ok(())
    }

    /// Add a comment (plain text in API v2).
    pub fn add_comment(&self, key: &str, body: &str) -> Result<Comment> {
        let url = self.api_url(&format!("issue/{key}/comment"));
        let resp = self.post(&url).send_text(&json!({ "body": body }).to_string())?;
        Ok(serde_json::from_str(&resp)?)
    }

    /// Assign an issue. Pass the Cloud `accountId` or the Server `name`; we set
    /// the correct field for each flavor.
    pub fn assign(&self, key: &str, assignee: &str) -> Result<()> {
        let url = self.api_url(&format!("issue/{key}/assignee"));
        let payload = if self.is_cloud() { json!({ "accountId": assignee }) } else { json!({ "name": assignee }) };
        self.put(&url).send_text(&payload.to_string())?;
        Ok(())
    }

    /// Create an issue; returns the new key.
    pub fn create_issue(
        &self,
        project_key: &str,
        issue_type: &str,
        summary: &str,
        description: Option<&str>,
        extra_fields: serde_json::Value,
    ) -> Result<String> {
        let url = self.api_url("issue");
        let mut fields = json!({
            "project": { "key": project_key },
            "issuetype": { "name": issue_type },
            "summary": summary,
        });
        if let Some(d) = description {
            fields["description"] = json!(d);
        }
        if let Some(obj) = extra_fields.as_object() {
            for (k, v) in obj {
                fields[k] = v.clone();
            }
        }
        let resp = self.post(&url).send_text(&json!({ "fields": fields }).to_string())?;
        let v: serde_json::Value = serde_json::from_str(&resp)?;
        Ok(v["key"].as_str().unwrap_or_default().to_string())
    }

    // ---- request plumbing ------------------------------------------------

    fn get(&self, url: &str) -> Req {
        self.ensure_fresh();
        Req::new(self.agent.get(url), &self.auth_header(), false)
    }
    fn post(&self, url: &str) -> Req {
        self.ensure_fresh();
        Req::new(self.agent.post(url), &self.auth_header(), true)
    }
    fn put(&self, url: &str) -> Req {
        self.ensure_fresh();
        Req::new(self.agent.put(url), &self.auth_header(), true)
    }
}

/// JQL for "my open work, freshest first".
pub fn my_issues_jql() -> String {
    "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC".to_string()
}

/// A request builder wrapping ureq with auth + JSON content-type + error mapping.
struct Req {
    inner: ureq::Request,
}

impl Req {
    fn new(mut inner: ureq::Request, auth_header: &str, json_body: bool) -> Self {
        inner = inner.set("Authorization", auth_header).set("Accept", "application/json");
        if json_body {
            inner = inner.set("Content-Type", "application/json");
        }
        Req { inner }
    }
    fn query(mut self, k: &str, v: &str) -> Self {
        self.inner = self.inner.query(k, v);
        self
    }
    fn call_text(self) -> Result<String> {
        map(self.inner.call())
    }
    fn send_text(self, body: &str) -> Result<String> {
        map(self.inner.send_string(body))
    }
}

/// Turn a ureq result into a String body or a useful error (status + snippet).
fn map(resp: Result<ureq::Response, ureq::Error>) -> Result<String> {
    match resp {
        Ok(r) => Ok(r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => {
            let snippet = r.into_string().unwrap_or_default();
            let snippet: String = snippet.chars().take(300).collect();
            bail!("Jira returned {code}: {snippet}")
        }
        Err(e) => bail!("Jira request failed (network/TLS): {e}"),
    }
}

/// Build the JSON for one transition field given the user's choice. `value` is
/// an allowedValue **id** for pick-lists, or raw text otherwise. Arrays (e.g.
/// labels, multi-select) are wrapped in a list.
pub fn transition_field_json(field: &crate::model::TransitionField, value: &str) -> serde_json::Value {
    let inner = if field.has_options() {
        json!({ "id": value })
    } else {
        json!(value)
    };
    if field.is_array() {
        json!([inner])
    } else {
        inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AllowedValue, FieldSchema, TransitionField};

    #[test]
    fn my_issues_jql_is_sane() {
        let jql = my_issues_jql();
        assert!(jql.contains("currentUser()"));
        assert!(jql.contains("statusCategory != Done"));
        assert!(jql.contains("ORDER BY updated DESC"));
    }

    #[test]
    fn transition_field_json_shapes() {
        // a pick-list field -> { "id": <id> }
        let select = TransitionField {
            required: true,
            name: "Resolution".into(),
            schema: FieldSchema { type_: "resolution".into(), items: String::new() },
            allowed_values: vec![AllowedValue { id: "10000".into(), name: "Done".into(), value: String::new() }],
        };
        assert_eq!(transition_field_json(&select, "10000"), json!({ "id": "10000" }));

        // a free-text field -> raw string
        let text = TransitionField { required: true, name: "Notes".into(), ..Default::default() };
        assert_eq!(transition_field_json(&text, "hello"), json!("hello"));

        // an array field -> wrapped list
        let multi = TransitionField {
            required: true,
            name: "Labels".into(),
            schema: FieldSchema { type_: "array".into(), items: "option".into() },
            allowed_values: vec![AllowedValue { id: "1".into(), name: "A".into(), value: String::new() }],
        };
        assert_eq!(transition_field_json(&multi, "1"), json!([{ "id": "1" }]));
    }
}
