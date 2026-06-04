//! An in-memory Jira for demo mode, offline development, and tests. Serves a
//! handful of realistic issues; writes (comment / transition / assign / create)
//! mutate the in-memory state and are reflected on the next read — so the entire
//! UI can be exercised, including the write paths, with no server.

use crate::api::Jira;
use crate::model::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

pub struct MockJira {
    state: Mutex<State>,
}

struct State {
    me: User,
    issues: Vec<Issue>,
    next: u32,
}

const FIXTURE: &str = r#"[
  {"key":"ACME-101","id":"1001","fields":{
    "summary":"Draft the Q3 partnership agreement","status":{"name":"In Progress","id":"3"},
    "assignee":{"displayName":"Marc Jones","accountId":"me"},"priority":{"name":"High","id":"2"},
    "issuetype":{"name":"Task","id":"10001"},"labels":["legal","q3"],"duedate":"2026-06-20",
    "description":"Prepare the first draft of the partnership agreement for review by the GC.",
    "comment":{"total":1,"comments":[
      {"id":"5001","author":{"displayName":"Dana Lee","accountId":"dana"},"body":"Please align the indemnification clause with the master template.","created":"2026-06-02T10:15:00.000+0000"}
    ]}}},
  {"key":"ACME-102","id":"1002","fields":{
    "summary":"Review NDA redlines from counterparty","status":{"name":"To Do","id":"1"},
    "assignee":{"displayName":"Marc Jones","accountId":"me"},"priority":{"name":"Medium","id":"3"},
    "issuetype":{"name":"Bug","id":"10004"},"labels":["nda"],"duedate":"2026-06-14",
    "description":"Counterparty returned redlines on sections 4 and 7. Compare to our fallback positions.",
    "comment":{"total":0,"comments":[]}}},
  {"key":"ACME-103","id":"1003","fields":{
    "summary":"Brief the team on the new data-retention policy","status":{"name":"To Do","id":"1"},
    "assignee":{"displayName":"Marc Jones","accountId":"me"},"priority":{"name":"Low","id":"4"},
    "issuetype":{"name":"Task","id":"10001"},"labels":["policy","privacy"],
    "description":"Schedule a 30-min session and circulate the one-pager beforehand.",
    "comment":{"total":0,"comments":[]}}},
  {"key":"ACME-104","id":"1004","fields":{
    "summary":"Close out the vendor onboarding checklist","status":{"name":"Done","id":"5"},
    "assignee":{"displayName":"Marc Jones","accountId":"me"},"priority":{"name":"Medium","id":"3"},
    "issuetype":{"name":"Task","id":"10001"},"labels":["vendor"],
    "description":"All items signed off.","comment":{"total":0,"comments":[]}}}
]"#;

impl Default for MockJira {
    fn default() -> Self {
        Self::new()
    }
}

impl MockJira {
    pub fn new() -> Self {
        let issues: Vec<Issue> = serde_json::from_str(FIXTURE).expect("valid fixture");
        let me = User {
            display_name: "Marc Jones".into(),
            account_id: "me".into(),
            name: "marc".into(),
            email: "marc@example.com".into(),
        };
        MockJira { state: Mutex::new(State { me, issues, next: 105 }) }
    }

    fn find<'a>(issues: &'a mut [Issue], key: &str) -> Option<&'a mut Issue> {
        issues.iter_mut().find(|i| i.key == key)
    }
}

/// The toy workflow: To Do → In Progress → Done, with a required `resolution`
/// field on the Done transition (so the dynamic-field dialog has something real).
fn transitions_for(status: &str) -> Vec<Transition> {
    let nr = |name: &str, id: &str| Some(NamedRef { name: name.into(), id: id.into() });
    let start = Transition { id: "21".into(), name: "Start Progress".into(), to: nr("In Progress", "3"), fields: BTreeMap::new() };
    let backlog = Transition { id: "11".into(), name: "Back to To Do".into(), to: nr("To Do", "1"), fields: BTreeMap::new() };
    let mut done_fields = BTreeMap::new();
    done_fields.insert(
        "resolution".to_string(),
        TransitionField {
            required: true,
            name: "Resolution".into(),
            schema: FieldSchema { type_: "resolution".into(), items: String::new() },
            allowed_values: vec![
                AllowedValue { id: "10000".into(), name: "Done".into(), value: String::new() },
                AllowedValue { id: "10001".into(), name: "Won't Do".into(), value: String::new() },
                AllowedValue { id: "10002".into(), name: "Duplicate".into(), value: String::new() },
            ],
        },
    );
    let done = Transition { id: "31".into(), name: "Done".into(), to: nr("Done", "5"), fields: done_fields };
    match status {
        "To Do" => vec![start],
        "In Progress" => vec![done, backlog],
        "Done" => vec![Transition { id: "11".into(), name: "Reopen".into(), to: nr("To Do", "1"), fields: BTreeMap::new() }],
        _ => vec![start],
    }
}

impl Jira for MockJira {
    fn myself(&self) -> Result<User> {
        Ok(self.state.lock().unwrap().me.clone())
    }

    fn my_issues(&self, max: u32) -> Result<Vec<Issue>> {
        let s = self.state.lock().unwrap();
        let mut v: Vec<Issue> = s.issues.iter().filter(|i| i.status() != "Done").cloned().collect();
        v.truncate(max as usize);
        Ok(v)
    }

    fn search(&self, _jql: &str, max: u32) -> Result<SearchResult> {
        let s = self.state.lock().unwrap();
        let issues: Vec<Issue> = s.issues.iter().take(max as usize).cloned().collect();
        let total = s.issues.len() as u32;
        Ok(SearchResult { issues, total, start_at: 0 })
    }

    fn issue(&self, key: &str) -> Result<Issue> {
        let s = self.state.lock().unwrap();
        s.issues.iter().find(|i| i.key == key).cloned().ok_or_else(|| anyhow::anyhow!("no such issue {key}"))
    }

    fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let i = self.issue(key)?;
        Ok(transitions_for(i.status()))
    }

    fn transition(&self, key: &str, transition_id: &str, _fields: Value) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let cur = s
            .issues
            .iter()
            .find(|i| i.key == key)
            .map(|i| i.status().to_string())
            .ok_or_else(|| anyhow::anyhow!("no such issue {key}"))?;
        let to = transitions_for(&cur)
            .into_iter()
            .find(|t| t.id == transition_id)
            .and_then(|t| t.to)
            .ok_or_else(|| anyhow::anyhow!("transition {transition_id} not available"))?;
        if let Some(i) = s.issues.iter_mut().find(|i| i.key == key) {
            i.fields.status = Some(to);
        }
        Ok(())
    }

    fn create_targets(&self) -> Result<Vec<CreateOption>> {
        let opt = |pk: &str, pn: &str, ty: &str, required: Vec<(String, TransitionField)>| CreateOption {
            project_key: pk.into(),
            project_name: pn.into(),
            type_name: ty.into(),
            required,
        };
        // "Matter" requires a Practice-area pick-list, to exercise dynamic fields.
        let practice = TransitionField {
            required: true,
            name: "Practice area".into(),
            schema: FieldSchema { type_: "option".into(), items: String::new() },
            allowed_values: vec![
                AllowedValue { id: "300".into(), name: "Corporate".into(), value: String::new() },
                AllowedValue { id: "301".into(), name: "Litigation".into(), value: String::new() },
                AllowedValue { id: "302".into(), name: "IP".into(), value: String::new() },
            ],
        };
        Ok(vec![
            opt("ACME", "Acme Legal", "Task", vec![]),
            opt("ACME", "Acme Legal", "Matter", vec![("customfield_20000".into(), practice)]),
            opt("ACME", "Acme Legal", "Bug", vec![]),
            opt("OPS", "Operations", "Task", vec![]),
        ])
    }

    fn assignable_users(&self, _key: &str) -> Result<Vec<User>> {
        let me = self.state.lock().unwrap().me.clone();
        Ok(vec![
            me,
            User { display_name: "Dana Lee".into(), account_id: "dana".into(), name: "dana".into(), email: "dana@example.com".into() },
            User { display_name: "Sam Patel".into(), account_id: "sam".into(), name: "sam".into(), email: String::new() },
        ])
    }

    fn edit_meta(&self, _key: &str) -> Result<Vec<(String, TransitionField)>> {
        let prio = TransitionField {
            required: false,
            name: "Priority".into(),
            schema: FieldSchema { type_: "priority".into(), items: String::new() },
            allowed_values: vec![
                AllowedValue { id: "2".into(), name: "High".into(), value: String::new() },
                AllowedValue { id: "3".into(), name: "Medium".into(), value: String::new() },
                AllowedValue { id: "4".into(), name: "Low".into(), value: String::new() },
            ],
        };
        let summary = TransitionField { required: false, name: "Summary".into(), ..Default::default() };
        let duedate = TransitionField {
            required: false,
            name: "Due date".into(),
            schema: FieldSchema { type_: "date".into(), items: String::new() },
            ..Default::default()
        };
        let severity = TransitionField {
            required: false,
            name: "Severity (custom field)".into(),
            schema: FieldSchema { type_: "option".into(), items: String::new() },
            allowed_values: vec![
                AllowedValue { id: "100".into(), name: "Sev-1".into(), value: String::new() },
                AllowedValue { id: "101".into(), name: "Sev-2".into(), value: String::new() },
            ],
        };
        let assignee = TransitionField {
            required: false,
            name: "Assignee".into(),
            schema: FieldSchema { type_: "user".into(), items: String::new() },
            ..Default::default()
        };
        let labels = TransitionField {
            required: false,
            name: "Labels".into(),
            schema: FieldSchema { type_: "array".into(), items: "string".into() },
            ..Default::default()
        };
        Ok(vec![
            ("summary".into(), summary),
            ("assignee".into(), assignee),
            ("priority".into(), prio),
            ("duedate".into(), duedate),
            ("labels".into(), labels),
            ("customfield_10010".into(), severity),
        ])
    }

    fn update_issue(&self, key: &str, fields: Value) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let i = Self::find(&mut s.issues, key).ok_or_else(|| anyhow::anyhow!("no such issue {key}"))?;
        if let Some(obj) = fields.as_object() {
            if let Some(v) = obj.get("summary").and_then(|v| v.as_str()) {
                i.fields.summary = v.to_string();
            }
            if let Some(v) = obj.get("duedate").and_then(|v| v.as_str()) {
                i.fields.duedate = Some(v.to_string());
            }
            if let Some(id) = obj.get("priority").and_then(|p| p.get("id")).and_then(|v| v.as_str()) {
                let name = match id { "2" => "High", "3" => "Medium", "4" => "Low", _ => "Medium" };
                i.fields.priority = Some(NamedRef { name: name.into(), id: id.into() });
            }
            if let Some(arr) = obj.get("labels").and_then(|v| v.as_array()) {
                i.fields.labels = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            }
        }
        Ok(())
    }

    fn add_comment(&self, key: &str, body: &str) -> Result<Comment> {
        let mut s = self.state.lock().unwrap();
        let me = s.me.clone();
        let i = Self::find(&mut s.issues, key).ok_or_else(|| anyhow::anyhow!("no such issue {key}"))?;
        let c = Comment {
            id: format!("c{}", i.fields.comment.as_ref().map(|p| p.comments.len()).unwrap_or(0) + 9000),
            author: Some(me),
            body: body.to_string(),
            created: "2026-06-04T12:00:00.000+0000".into(),
        };
        let page = i.fields.comment.get_or_insert_with(CommentPage::default);
        page.comments.push(c.clone());
        page.total = page.comments.len() as u32;
        Ok(c)
    }

    fn add_request_comment(&self, key: &str, body: &str, public: bool) -> Result<()> {
        // Demo: tag the comment so the public/internal distinction is visible.
        let tag = if public { "[Reply to customer] " } else { "[Internal note] " };
        self.add_comment(key, &format!("{tag}{body}")).map(|_| ())
    }

    fn assign(&self, key: &str, assignee: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let i = Self::find(&mut s.issues, key).ok_or_else(|| anyhow::anyhow!("no such issue {key}"))?;
        i.fields.assignee = Some(User { display_name: assignee.into(), account_id: assignee.into(), name: assignee.into(), email: String::new() });
        Ok(())
    }

    fn create_issue(&self, project_key: &str, issue_type: &str, summary: &str, description: Option<&str>, _extra: Value) -> Result<String> {
        let mut s = self.state.lock().unwrap();
        let key = format!("{project_key}-{}", s.next);
        let id = format!("{}", 2000 + s.next);
        s.next += 1;
        let mut fields = Fields { summary: summary.to_string(), ..Default::default() };
        fields.status = Some(NamedRef { name: "To Do".into(), id: "1".into() });
        fields.issuetype = Some(NamedRef { name: issue_type.into(), id: "0".into() });
        fields.assignee = Some(s.me.clone());
        fields.description = description.map(|d| d.to_string());
        s.issues.push(Issue { key: key.clone(), id, fields });
        Ok(key)
    }

    fn browse_url(&self, key: &str) -> String {
        format!("https://demo.atlassian.net/browse/{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_serves_and_mutates() {
        let m = MockJira::new();
        assert_eq!(m.myself().unwrap().display_name, "Marc Jones");

        // my_issues excludes Done
        let open = m.my_issues(50).unwrap();
        assert_eq!(open.len(), 3);
        assert!(open.iter().all(|i| i.status() != "Done"));

        // comment write is reflected on re-read
        m.add_comment("ACME-102", "Looks good to me.").unwrap();
        let i = m.issue("ACME-102").unwrap();
        assert_eq!(i.fields.comment.unwrap().comments.last().unwrap().body, "Looks good to me.");

        // transition To Do -> In Progress
        let ts = m.transitions("ACME-102").unwrap();
        assert_eq!(ts[0].name, "Start Progress");
        m.transition("ACME-102", "21", Value::Null).unwrap();
        assert_eq!(m.issue("ACME-102").unwrap().status(), "In Progress");

        // the Done transition advertises a required field
        let ts = m.transitions("ACME-102").unwrap();
        let done = ts.iter().find(|t| t.name == "Done").unwrap();
        assert!(done.fields.get("resolution").unwrap().required);

        // create returns a fresh key that then exists
        let key = m.create_issue("ACME", "Task", "New thing", Some("desc"), Value::Null).unwrap();
        assert_eq!(key, "ACME-105");
        assert_eq!(m.issue(&key).unwrap().summary(), "New thing");
    }
}
