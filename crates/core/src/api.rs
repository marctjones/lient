//! The `Jira` trait — the seam the UI talks to. The real [`JiraClient`] hits the
//! network; [`crate::mock::MockJira`] serves fixtures so the entire app can run
//! and be exercised with no server (demo mode, tests, offline development).
//!
//! Object-safe and `Send + Sync` so the GUI can run calls on worker threads.

use crate::client::JiraClient;
use crate::model::{Comment, CreateOption, Issue, SearchResult, Transition, TransitionField, User};
use anyhow::Result;
use serde_json::Value;

pub trait Jira: Send + Sync {
    fn myself(&self) -> Result<User>;
    fn my_issues(&self, max: u32) -> Result<Vec<Issue>>;
    fn search(&self, jql: &str, max: u32) -> Result<SearchResult>;
    fn issue(&self, key: &str) -> Result<Issue>;
    fn transitions(&self, key: &str) -> Result<Vec<Transition>>;
    fn transition(&self, key: &str, transition_id: &str, fields: Value) -> Result<()>;
    /// Projects × issue types you can create in, with each type's required fields.
    fn create_targets(&self) -> Result<Vec<CreateOption>>;
    /// Users who can be assigned this issue (for the assignee picker).
    fn assignable_users(&self, key: &str) -> Result<Vec<User>>;
    /// Fields editable on this issue (standard + custom), with their metadata.
    fn edit_meta(&self, key: &str) -> Result<Vec<(String, TransitionField)>>;
    /// Update issue fields. `fields` is the Jira `fields` object.
    fn update_issue(&self, key: &str, fields: Value) -> Result<()>;
    fn add_comment(&self, key: &str, body: &str) -> Result<Comment>;
    /// JSM request comment with visibility: `public = true` replies to the
    /// customer, `false` is an internal note. JSM-only.
    fn add_request_comment(&self, key: &str, body: &str, public: bool) -> Result<()>;
    fn assign(&self, key: &str, assignee: &str) -> Result<()>;
    fn create_issue(
        &self,
        project_key: &str,
        issue_type: &str,
        summary: &str,
        description: Option<&str>,
        extra_fields: Value,
    ) -> Result<String>;
    fn browse_url(&self, key: &str) -> String;
}

/// The real REST client satisfies the trait by delegating to its inherent methods.
impl Jira for JiraClient {
    fn myself(&self) -> Result<User> {
        JiraClient::myself(self)
    }
    fn my_issues(&self, max: u32) -> Result<Vec<Issue>> {
        JiraClient::my_issues(self, max)
    }
    fn search(&self, jql: &str, max: u32) -> Result<SearchResult> {
        JiraClient::search(self, jql, max)
    }
    fn issue(&self, key: &str) -> Result<Issue> {
        JiraClient::issue(self, key)
    }
    fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        JiraClient::transitions(self, key)
    }
    fn transition(&self, key: &str, transition_id: &str, fields: Value) -> Result<()> {
        JiraClient::transition(self, key, transition_id, fields)
    }
    fn create_targets(&self) -> Result<Vec<CreateOption>> {
        JiraClient::create_targets(self)
    }
    fn assignable_users(&self, key: &str) -> Result<Vec<User>> {
        JiraClient::assignable_users(self, key)
    }
    fn edit_meta(&self, key: &str) -> Result<Vec<(String, TransitionField)>> {
        JiraClient::edit_meta(self, key)
    }
    fn update_issue(&self, key: &str, fields: Value) -> Result<()> {
        JiraClient::update_issue(self, key, fields)
    }
    fn add_comment(&self, key: &str, body: &str) -> Result<Comment> {
        JiraClient::add_comment(self, key, body)
    }
    fn add_request_comment(&self, key: &str, body: &str, public: bool) -> Result<()> {
        JiraClient::add_request_comment(self, key, body, public)
    }
    fn assign(&self, key: &str, assignee: &str) -> Result<()> {
        JiraClient::assign(self, key, assignee)
    }
    fn create_issue(
        &self,
        project_key: &str,
        issue_type: &str,
        summary: &str,
        description: Option<&str>,
        extra_fields: Value,
    ) -> Result<String> {
        JiraClient::create_issue(self, project_key, issue_type, summary, description, extra_fields)
    }
    fn browse_url(&self, key: &str) -> String {
        self.config().browse_url(key)
    }
}
