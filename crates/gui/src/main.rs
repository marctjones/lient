// Lient — native Slint Jira daily-driver over lient-core.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use lient_core::client::transition_field_json;
use lient_core::model::{Comment, Issue, Transition, TransitionField};
use lient_core::{Auth, Jira, JiraClient, JiraConfig, MockJira};
use slint::{ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

slint::include_modules!();

/// The live data source, swappable at runtime (e.g. after the login wizard).
/// Held in an `Rc<RefCell<…>>` on the UI thread; we clone the inner `Arc` out
/// before handing it to a worker thread.
type JiraCell = Rc<RefCell<Arc<dyn Jira>>>;

// UI-thread state for the open issue's transitions and the in-progress dialog.
// Kept in thread-locals because the worker `done` closures must be `Send` and so
// can't capture `Rc` state (same pattern Noet uses for fold state).
thread_local! {
    static TRANSITIONS: RefCell<Vec<Transition>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

/// A transition awaiting its required fields in the dialog.
struct Pending {
    key: String,
    tid: String,
    fields: Vec<PField>,
}
struct PField {
    key: String,            // Jira field key, e.g. "resolution"
    field: TransitionField, // metadata, for building the submit JSON
    value: String,          // chosen allowedValue **id**, or raw text
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let force_demo = std::env::var("LIENT_DEMO").is_ok() || std::env::args().any(|a| a == "--demo");

    let ui = AppWindow::new()?;

    // Decide the initial source + view:
    //   --demo            → demo data, main view
    //   config present    → real client, main view
    //   nothing           → demo placeholder, but show the LOGIN wizard
    let (cell, demo, view): (JiraCell, bool, &str) = if force_demo {
        (Rc::new(RefCell::new(Arc::new(MockJira::new()))), true, "main")
    } else {
        match JiraConfig::load() {
            Ok(cfg) => (Rc::new(RefCell::new(Arc::new(JiraClient::new(cfg)))), false, "main"),
            Err(_) => (Rc::new(RefCell::new(Arc::new(MockJira::new()))), true, "login"),
        }
    };
    ui.set_demo(demo);
    ui.set_view(view.into());

    // ---- main-view callbacks --------------------------------------------
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_refresh(move || load_list(&ui_w.unwrap(), cell.borrow().clone()));
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_select(move |key| {
            let ui = ui_w.unwrap();
            ui.set_selected_key(key.clone());
            load_detail(&ui, cell.borrow().clone(), key.to_string());
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_add_comment(move |key, body| {
            if body.trim().is_empty() {
                return;
            }
            let ui = ui_w.unwrap();
            let jira = cell.borrow().clone();
            let (k, b) = (key.to_string(), body.to_string());
            run(&ui, jira.clone(), move |j| j.add_comment(&k, &b).map(|_| k), move |ui, k| {
                load_detail(&ui, jira, k);
            });
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_do_transition(move |key, tid| {
            let ui = ui_w.unwrap();
            let (k, t) = (key.to_string(), tid.to_string());
            // Required fields for this transition (from the open issue's metadata).
            let req: Vec<(String, TransitionField)> = TRANSITIONS.with(|ts| {
                ts.borrow()
                    .iter()
                    .find(|x| x.id == t)
                    .map(|x| x.fields.iter().filter(|(_, f)| f.required).map(|(fk, f)| (fk.clone(), f.clone())).collect())
                    .unwrap_or_default()
            });
            if req.is_empty() {
                // no required fields → just do it
                execute_transition(&ui, cell.borrow().clone(), k, t, serde_json::Value::Null);
            } else {
                open_transition_dialog(&ui, k, t, req);
            }
        });
    }
    {
        ui.on_field_changed(move |i, value| {
            PENDING.with(|p| {
                if let Some(pend) = p.borrow_mut().as_mut() {
                    if let Some(pf) = pend.fields.get_mut(i as usize) {
                        // pick-lists report the chosen label → map back to its id
                        pf.value = if pf.field.has_options() {
                            pf.field.allowed_values.iter().find(|a| a.label() == value.as_str()).map(|a| a.id.clone()).unwrap_or_else(|| value.to_string())
                        } else {
                            value.to_string()
                        };
                    }
                }
            });
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_submit_transition(move || {
            let ui = ui_w.unwrap();
            ui.set_dialog_open(false);
            if let Some(pend) = PENDING.with(|p| p.borrow_mut().take()) {
                let mut map = serde_json::Map::new();
                for pf in &pend.fields {
                    map.insert(pf.key.clone(), transition_field_json(&pf.field, &pf.value));
                }
                execute_transition(&ui, cell.borrow().clone(), pend.key, pend.tid, serde_json::Value::Object(map));
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_cancel_transition(move || {
            PENDING.with(|p| *p.borrow_mut() = None);
            ui_w.unwrap().set_dialog_open(false);
        });
    }
    {
        let cell = cell.clone();
        ui.on_open_browser(move |key| {
            let _ = open::that(cell.borrow().browse_url(&key));
        });
    }

    // ---- login-wizard callbacks -----------------------------------------
    {
        let ui_w = ui.as_weak();
        ui.on_test_connection(move || {
            let ui = ui_w.unwrap();
            match config_from_ui(&ui) {
                Err(e) => ui.set_test_status(format!("✗ {e}").into()),
                Ok(cfg) => {
                    ui.set_testing(true);
                    ui.set_test_status("contacting Jira…".into());
                    let w = ui.as_weak();
                    std::thread::spawn(move || {
                        let res = JiraClient::new(cfg).myself();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = w.upgrade() {
                                ui.set_testing(false);
                                match res {
                                    Ok(me) => ui.set_test_status(format!("✓ Connected as {}", me.display_name).into()),
                                    Err(e) => ui.set_test_status(format!("✗ {e:#}").into()),
                                }
                            }
                        });
                    });
                }
            }
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_save_login(move || {
            let ui = ui_w.unwrap();
            match config_from_ui(&ui) {
                Err(e) => ui.set_test_status(format!("✗ {e}").into()),
                Ok(cfg) => {
                    if let Err(e) = cfg.save() {
                        ui.set_test_status(format!("✗ couldn't save config: {e}").into());
                        return;
                    }
                    *cell.borrow_mut() = Arc::new(JiraClient::new(cfg));
                    ui.set_demo(false);
                    ui.set_test_status("".into());
                    ui.set_view("main".into());
                    let jira = cell.borrow().clone();
                    load_me(&ui, jira.clone());
                    load_list(&ui, jira);
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_show_login(move || ui_w.unwrap().set_view("login".into()));
    }

    // ---- initial load ----------------------------------------------------
    if view == "main" {
        load_me(&ui, cell.borrow().clone());
        load_list(&ui, cell.borrow().clone());
    }

    ui.run()?;
    Ok(())
}

/// Build a [`JiraConfig`] from the login-wizard fields.
fn config_from_ui(ui: &AppWindow) -> anyhow::Result<JiraConfig> {
    let url = ui.get_login_url().trim().to_string();
    if url.is_empty() {
        anyhow::bail!("Enter your Jira site URL");
    }
    let token = ui.get_login_token().trim().to_string();
    let auth = match ui.get_login_method().as_str() {
        "token" => {
            let email = ui.get_login_email().trim().to_string();
            if email.is_empty() || token.is_empty() {
                anyhow::bail!("Email and API token are required");
            }
            Auth::Basic { email, token }
        }
        "pat" => {
            if token.is_empty() {
                anyhow::bail!("Personal Access Token is required");
            }
            Auth::Bearer { token }
        }
        "password" => Auth::Password {
            username: ui.get_login_username().trim().to_string(),
            password: ui.get_login_password().to_string(),
        },
        "raw" => Auth::Raw { header: ui.get_login_raw().trim().to_string() },
        other => anyhow::bail!("unknown method {other}"),
    };
    Ok(JiraConfig { base_url: url, auth, api_version: "2".into() })
}

/// Run `work` on a worker thread, then `done` back on the UI thread.
fn run<T, W, D>(ui: &AppWindow, jira: Arc<dyn Jira>, work: W, done: D)
where
    T: Send + 'static,
    W: FnOnce(&dyn Jira) -> anyhow::Result<T> + Send + 'static,
    D: FnOnce(AppWindow, T) + Send + 'static,
{
    let w = ui.as_weak();
    std::thread::spawn(move || {
        let res = work(jira.as_ref());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = w.upgrade() else { return };
            match res {
                Ok(v) => done(ui, v),
                Err(e) => ui.set_error(format!("{e:#}").into()),
            }
        });
    });
}

fn load_me(ui: &AppWindow, jira: Arc<dyn Jira>) {
    run(ui, jira, |j| j.myself(), |ui, me| {
        let id = if me.email.is_empty() { me.display_name.clone() } else { format!("{} <{}>", me.display_name, me.email) };
        ui.set_me(id.into());
    });
}

fn load_list(ui: &AppWindow, jira: Arc<dyn Jira>) {
    ui.set_loading(true);
    run(ui, jira, |j| j.my_issues(100), |ui, issues| {
        ui.set_loading(false);
        ui.set_error("".into());
        let rows: Vec<IssueRow> = issues.iter().map(to_row).collect();
        ui.set_issues(ModelRc::new(VecModel::from(rows)));
    });
}

fn load_detail(ui: &AppWindow, jira: Arc<dyn Jira>, key: String) {
    run(
        ui,
        jira,
        move |j| {
            let issue = j.issue(&key)?;
            let trans = j.transitions(&key).unwrap_or_default();
            Ok((issue, trans))
        },
        |ui, (issue, trans)| apply_detail(&ui, &issue, &trans),
    );
}

/// Run a transition on a worker thread, then refresh detail + list.
fn execute_transition(ui: &AppWindow, jira: Arc<dyn Jira>, key: String, tid: String, fields: serde_json::Value) {
    run(
        ui,
        jira.clone(),
        move |j| j.transition(&key, &tid, fields).map(|_| key),
        move |ui, k| {
            load_detail(&ui, jira.clone(), k);
            load_list(&ui, jira);
        },
    );
}

/// Populate and open the required-field dialog for a transition.
fn open_transition_dialog(ui: &AppWindow, key: String, tid: String, req: Vec<(String, TransitionField)>) {
    let mut pfields = Vec::new();
    let mut rows: Vec<FieldRow> = Vec::new();
    for (fkey, f) in req {
        let is_select = f.has_options();
        let options: Vec<SharedString> = f.allowed_values.iter().map(|a| a.label().into()).collect();
        let (default_id, default_label) = if is_select {
            f.allowed_values.first().map(|a| (a.id.clone(), a.label().to_string())).unwrap_or_default()
        } else {
            (String::new(), String::new())
        };
        rows.push(FieldRow {
            name: f.name.clone().into(),
            options: ModelRc::new(VecModel::from(options)),
            is_select,
            value: default_label.into(),
        });
        pfields.push(PField { key: fkey, field: f, value: default_id });
    }
    PENDING.with(|p| *p.borrow_mut() = Some(Pending { key: key.clone(), tid, fields: pfields }));
    ui.set_dialog_title(format!("Confirm: {key}").into());
    ui.set_dialog_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_dialog_open(true);
}

fn to_row(i: &Issue) -> IssueRow {
    IssueRow {
        key: i.key.clone().into(),
        summary: i.summary().into(),
        status: i.status().into(),
        priority: i.priority().into(),
        itype: i.issue_type().into(),
        assignee: i.assignee().into(),
    }
}

fn apply_detail(ui: &AppWindow, issue: &Issue, trans: &[Transition]) {
    // Stash the full transition metadata for the dialog (required fields / options).
    TRANSITIONS.with(|t| *t.borrow_mut() = trans.to_vec());
    ui.set_d_summary(issue.summary().into());
    ui.set_d_status(issue.status().into());
    let mut meta = format!("{} · {} · {}", issue.issue_type(), issue.priority(), issue.assignee());
    if !issue.fields.labels.is_empty() {
        meta.push_str(&format!(" · {}", issue.fields.labels.join(", ")));
    }
    if let Some(due) = &issue.fields.duedate {
        meta.push_str(&format!(" · due {due}"));
    }
    ui.set_d_meta(meta.into());
    ui.set_d_desc(issue.fields.description.clone().unwrap_or_default().into());

    let comments: Vec<CommentRow> = issue
        .fields
        .comment
        .as_ref()
        .map(|p| p.comments.iter().map(to_comment).collect())
        .unwrap_or_default();
    ui.set_comments(ModelRc::new(VecModel::from(comments)));

    let trows: Vec<TransitionRow> = trans
        .iter()
        .map(|t| {
            let requires: Vec<&str> = t.fields.iter().filter(|(_, f)| f.required).map(|(k, _)| k.as_str()).collect();
            TransitionRow {
                id: t.id.clone().into(),
                name: t.name.clone().into(),
                to: t.to.as_ref().map(|n| n.name.clone()).unwrap_or_default().into(),
                requires: requires.join(", ").into(),
            }
        })
        .collect();
    ui.set_transitions(ModelRc::new(VecModel::from(trows)));
}

fn to_comment(c: &Comment) -> CommentRow {
    CommentRow {
        author: c.author.as_ref().map(|a| a.display_name.clone()).unwrap_or_default().into(),
        body: c.body.clone().into(),
        created: c.created.chars().take(10).collect::<String>().into(),
    }
}
