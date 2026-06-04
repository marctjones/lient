//! Typed views over the Jira REST API v2 JSON. Only the fields a daily-driver
//! needs are modeled; everything else is ignored (serde tolerates extra keys).

use serde::Deserialize;

/// Result of a JQL search (`/search`).
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub total: u32,
    #[serde(default, rename = "startAt")]
    pub start_at: u32,
}

/// A single issue, flattened to the bits the UI shows.
#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub key: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub fields: Fields,
}

impl Issue {
    pub fn summary(&self) -> &str {
        &self.fields.summary
    }
    pub fn status(&self) -> &str {
        self.fields.status.as_ref().map(|s| s.name.as_str()).unwrap_or("")
    }
    pub fn assignee(&self) -> &str {
        self.fields.assignee.as_ref().map(|u| u.display_name.as_str()).unwrap_or("Unassigned")
    }
    pub fn priority(&self) -> &str {
        self.fields.priority.as_ref().map(|p| p.name.as_str()).unwrap_or("")
    }
    pub fn issue_type(&self) -> &str {
        self.fields.issuetype.as_ref().map(|t| t.name.as_str()).unwrap_or("")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Fields {
    #[serde(default)]
    pub summary: String,
    pub status: Option<NamedRef>,
    pub assignee: Option<User>,
    pub reporter: Option<User>,
    pub priority: Option<NamedRef>,
    pub issuetype: Option<NamedRef>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub created: String,
    pub duedate: Option<String>,
    /// In API v2 the description is a plain/wiki string (v3 would be ADF JSON).
    pub description: Option<String>,
    pub comment: Option<CommentPage>,
}

/// Any `{ name, id }`-shaped reference (status, priority, issue type, …).
#[derive(Debug, Clone, Deserialize)]
pub struct NamedRef {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    #[serde(default, rename = "displayName")]
    pub display_name: String,
    /// Cloud uses `accountId`; Server/DC uses `name` — keep both.
    #[serde(default, rename = "accountId")]
    pub account_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "emailAddress")]
    pub email: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommentPage {
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    #[serde(default)]
    pub id: String,
    pub author: Option<User>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created: String,
}

/// A workflow transition available from an issue's current status.
#[derive(Debug, Clone, Deserialize)]
pub struct Transition {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The status the issue moves to.
    pub to: Option<NamedRef>,
    /// Fields this transition requires/allows (present with
    /// `expand=transitions.fields`). The UI renders required ones.
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, TransitionField>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransitionField {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub schema: FieldSchema,
    /// Permitted values for select-type fields (resolution, priority, options…).
    #[serde(default, rename = "allowedValues")]
    pub allowed_values: Vec<AllowedValue>,
}

impl TransitionField {
    /// True when this field is a pick-list (render a dropdown, not a text box).
    pub fn has_options(&self) -> bool {
        !self.allowed_values.is_empty()
    }
    pub fn is_array(&self) -> bool {
        self.schema.type_ == "array"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FieldSchema {
    #[serde(default, rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub items: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AllowedValue {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

impl AllowedValue {
    /// Human label (Jira uses `name` for most, `value` for custom options).
    pub fn label(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else {
            &self.value
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransitionsResponse {
    #[serde(default)]
    pub transitions: Vec<Transition>,
}

/// `/issue/{key}/editmeta` — the fields editable on this issue (standard + custom),
/// each with the same shape as a transition field (name / schema / allowedValues).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EditMeta {
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, TransitionField>,
}

/// `/issue/createmeta?expand=projects.issuetypes` — the projects you can create
/// in and the issue types each offers.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateMeta {
    #[serde(default)]
    pub projects: Vec<CreateProject>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateProject {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub issuetypes: Vec<NamedRef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_result() {
        let json = r#"{
            "startAt": 0, "total": 2,
            "issues": [
                {"key":"ENG-12","id":"1001","fields":{
                    "summary":"Fix the login bug",
                    "status":{"name":"In Progress","id":"3"},
                    "assignee":{"displayName":"Alice Smith","accountId":"a1"},
                    "priority":{"name":"High","id":"2"},
                    "issuetype":{"name":"Bug","id":"10004"},
                    "labels":["urgent","auth"],
                    "duedate":"2026-06-30"
                }},
                {"key":"ENG-13","fields":{"summary":"No assignee here"}}
            ]
        }"#;
        let r: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.total, 2);
        assert_eq!(r.issues.len(), 2);

        let i = &r.issues[0];
        assert_eq!(i.key, "ENG-12");
        assert_eq!(i.summary(), "Fix the login bug");
        assert_eq!(i.status(), "In Progress");
        assert_eq!(i.assignee(), "Alice Smith");
        assert_eq!(i.priority(), "High");
        assert_eq!(i.issue_type(), "Bug");
        assert_eq!(i.fields.labels, vec!["urgent", "auth"]);
        assert_eq!(i.fields.duedate.as_deref(), Some("2026-06-30"));

        // missing optional fields degrade gracefully, never panic
        assert_eq!(r.issues[1].assignee(), "Unassigned");
        assert_eq!(r.issues[1].status(), "");
    }

    #[test]
    fn parses_transitions_with_required_fields() {
        let json = r#"{"transitions":[
            {"id":"21","name":"Start Progress","to":{"name":"In Progress"},"fields":{}},
            {"id":"31","name":"Done","to":{"name":"Done"},"fields":{
                "resolution":{"required":true,"name":"Resolution"},
                "comment":{"required":false,"name":"Comment"}
            }}
        ]}"#;
        let r: TransitionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.transitions.len(), 2);
        let done = &r.transitions[1];
        assert_eq!(done.name, "Done");
        assert_eq!(done.to.as_ref().unwrap().name, "Done");
        assert!(done.fields.get("resolution").unwrap().required);
        assert!(!done.fields.get("comment").unwrap().required);
    }
}
