//! The Jira REST client. Blocking (ureq) — fine for a desktop app; each call is
//! a quick request and the GUI runs them off the UI thread. Read + light-write
//! only (the Lient scope): search, fetch, transition, comment, assign, create.

use crate::config::JiraConfig;
use crate::model::*;
use anyhow::{bail, Result};
use serde_json::json;

pub struct JiraClient {
    cfg: JiraConfig,
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
        Self { cfg, agent }
    }

    pub fn config(&self) -> &JiraConfig {
        &self.cfg
    }

    // ---- reads -----------------------------------------------------------

    /// Issues assigned to the authenticated user, most-recently-updated first.
    pub fn my_issues(&self, max: u32) -> Result<Vec<Issue>> {
        Ok(self.search(&my_issues_jql(), max)?.issues)
    }

    /// Arbitrary JQL search.
    pub fn search(&self, jql: &str, max: u32) -> Result<SearchResult> {
        let url = self.cfg.api_url("search");
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
        let url = self.cfg.api_url(&format!("issue/{key}"));
        let body = self.get(&url).query("fields", DETAIL_FIELDS).call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Confirm the connection works and return the current user.
    pub fn myself(&self) -> Result<User> {
        let url = self.cfg.api_url("myself");
        let body = self.get(&url).call_text()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Available workflow transitions, with the fields each requires.
    pub fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = self.cfg.api_url(&format!("issue/{key}/transitions"));
        let body = self.get(&url).query("expand", "transitions.fields").call_text()?;
        let resp: TransitionsResponse = serde_json::from_str(&body)?;
        Ok(resp.transitions)
    }

    // ---- light writes ----------------------------------------------------

    /// Move an issue through a transition. `extra_fields` carries any required
    /// screen fields (e.g. `{"resolution":{"name":"Done"}}`), rendered by the UI.
    pub fn transition(&self, key: &str, transition_id: &str, extra_fields: serde_json::Value) -> Result<()> {
        let url = self.cfg.api_url(&format!("issue/{key}/transitions"));
        let mut payload = json!({ "transition": { "id": transition_id } });
        if let Some(obj) = extra_fields.as_object() {
            if !obj.is_empty() {
                payload["fields"] = extra_fields;
            }
        }
        self.post(&url).send_text(&payload.to_string())?; // 204 No Content on success
        Ok(())
    }

    /// Add a comment (plain text in API v2).
    pub fn add_comment(&self, key: &str, body: &str) -> Result<Comment> {
        let url = self.cfg.api_url(&format!("issue/{key}/comment"));
        let resp = self.post(&url).send_text(&json!({ "body": body }).to_string())?;
        Ok(serde_json::from_str(&resp)?)
    }

    /// Assign an issue. Pass the Cloud `accountId` or the Server `name`; we set
    /// the correct field for each flavor.
    pub fn assign(&self, key: &str, assignee: &str) -> Result<()> {
        let url = self.cfg.api_url(&format!("issue/{key}/assignee"));
        // Cloud (API token / OAuth) identifies users by accountId; Server/DC by name.
        let payload = match &self.cfg.auth {
            crate::config::Auth::Basic { .. } | crate::config::Auth::OAuth { .. } => {
                json!({ "accountId": assignee })
            }
            _ => json!({ "name": assignee }),
        };
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
        let url = self.cfg.api_url("issue");
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
        Req::new(self.agent.get(url), &self.cfg, false)
    }
    fn post(&self, url: &str) -> Req {
        Req::new(self.agent.post(url), &self.cfg, true)
    }
    fn put(&self, url: &str) -> Req {
        Req::new(self.agent.put(url), &self.cfg, true)
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
    fn new(mut inner: ureq::Request, cfg: &JiraConfig, json_body: bool) -> Self {
        inner = inner
            .set("Authorization", &cfg.auth_header())
            .set("Accept", "application/json");
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
