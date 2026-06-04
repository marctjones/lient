// Lient — native Slint Jira daily-driver over lient-core.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use lient_core::client::transition_field_json;
use lient_core::model::{AllowedValue, Comment, Issue, Transition, TransitionField, User};
use lient_core::{Auth, Jira, JiraClient, JiraConfig, MockJira};
use slint::{ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

slint::include_modules!();

/// Command-palette entries: (id dispatched in on_palette_run, display label).
const COMMANDS: &[(&str, &str)] = &[
    ("new", "New issue"),
    ("refresh", "Refresh My Work"),
    ("edit", "Edit issue fields"),
    ("open", "Open issue in browser"),
    ("account", "Account / sign in"),
    ("quit", "Quit Lient"),
];

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

/// What the shared field dialog will do on submit.
enum PendingKind {
    /// Move through a transition (submit all fields).
    Transition { tid: String },
    /// Edit issue fields (submit only the fields the user touched).
    Edit,
    /// Create a new issue (fields: __projtype / summary / description).
    Create,
}

/// A pending field dialog (transition required-fields, or an edit).
struct Pending {
    kind: PendingKind,
    key: String,
    fields: Vec<PField>,
}
struct PField {
    key: String,            // Jira field key, e.g. "resolution" / "customfield_10010"
    field: TransitionField, // metadata, for building the submit JSON
    value: String,          // chosen allowedValue **id**, or raw text
    touched: bool,          // did the user change it? (edit mode submits only these)
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
            // "Reply to customer" → JSM public comment; otherwise a normal comment
            // (which is an internal note on JSM, a plain comment elsewhere).
            let public = ui.get_reply_public();
            let (k, b) = (key.to_string(), body.to_string());
            run(
                &ui,
                jira.clone(),
                move |j| {
                    if public {
                        j.add_request_comment(&k, &b, true)?;
                    } else {
                        j.add_comment(&k, &b)?;
                    }
                    Ok(k)
                },
                move |ui, k| load_detail(&ui, jira, k),
            );
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
                        pf.touched = true;
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
            let Some(pend) = PENDING.with(|p| p.borrow_mut().take()) else { return };
            match pend.kind {
                PendingKind::Transition { tid } => {
                    // transitions submit all (required) fields
                    let mut map = serde_json::Map::new();
                    for pf in &pend.fields {
                        map.insert(pf.key.clone(), transition_field_json(&pf.field, &pf.value));
                    }
                    execute_transition(&ui, cell.borrow().clone(), pend.key, tid, serde_json::Value::Object(map));
                }
                PendingKind::Edit => {
                    // edits submit only the fields the user changed. Assignee goes
                    // through the dedicated /assignee endpoint (handles Cloud vs
                    // Server id shape); everything else through PUT /issue.
                    let mut map = serde_json::Map::new();
                    let mut assignee: Option<String> = None;
                    for pf in pend.fields.iter().filter(|f| f.touched) {
                        if pf.key == "assignee" {
                            assignee = Some(pf.value.clone());
                        } else if pf.key == "labels" {
                            // comma-separated text → array of label strings
                            let arr: Vec<serde_json::Value> = pf
                                .value
                                .split(',')
                                .map(|s| s.trim())
                                .filter(|s| !s.is_empty())
                                .map(|s| serde_json::Value::String(s.to_string()))
                                .collect();
                            map.insert("labels".into(), serde_json::Value::Array(arr));
                        } else {
                            map.insert(pf.key.clone(), transition_field_json(&pf.field, &pf.value));
                        }
                    }
                    if assignee.is_none() && map.is_empty() {
                        return;
                    }
                    let jira = cell.borrow().clone();
                    let key = pend.key;
                    run(
                        &ui,
                        jira.clone(),
                        move |j| {
                            if let Some(a) = &assignee {
                                j.assign(&key, a)?;
                            }
                            if !map.is_empty() {
                                j.update_issue(&key, serde_json::Value::Object(map))?;
                            }
                            Ok(key)
                        },
                        move |ui, k| {
                            load_detail(&ui, jira.clone(), k);
                            load_list(&ui, jira);
                        },
                    );
                }
                PendingKind::Create => {
                    let (mut project, mut itype, mut summary, mut desc) = (String::new(), String::new(), String::new(), String::new());
                    for pf in &pend.fields {
                        match pf.key.as_str() {
                            "__projtype" => {
                                let mut parts = pf.value.splitn(2, '|');
                                project = parts.next().unwrap_or("").to_string();
                                itype = parts.next().unwrap_or("").to_string();
                            }
                            "summary" => summary = pf.value.clone(),
                            "description" => desc = pf.value.clone(),
                            _ => {}
                        }
                    }
                    if summary.trim().is_empty() {
                        ui.set_error("Summary is required to create an issue.".into());
                        return;
                    }
                    let jira = cell.borrow().clone();
                    run(
                        &ui,
                        jira.clone(),
                        move |j| {
                            let d = if desc.trim().is_empty() { None } else { Some(desc.as_str()) };
                            j.create_issue(&project, &itype, &summary, d, serde_json::Value::Null)
                        },
                        move |ui, key| {
                            load_list(&ui, jira.clone());
                            ui.set_selected_key(key.clone().into());
                            ui.set_error(format!("Created {key}").into());
                            load_detail(&ui, jira, key);
                        },
                    );
                }
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
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_edit_issue(move |key| {
            let ui = ui_w.unwrap();
            let jira = cell.borrow().clone();
            let k = key.to_string();
            run(
                &ui,
                jira,
                move |j| {
                    let metas = j.edit_meta(&k)?;
                    let issue = j.issue(&k)?; // for prefilling current values
                    let users = j.assignable_users(&k).unwrap_or_default(); // assignee picker
                    Ok((k, metas, issue, users))
                },
                |ui, (k, metas, issue, users)| open_edit_dialog(&ui, k, metas, &issue, &users),
            );
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_new_issue(move || {
            let ui = ui_w.unwrap();
            let jira = cell.borrow().clone();
            run(&ui, jira, |j| j.create_targets(), |ui, targets| open_create_dialog(&ui, targets));
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
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    ui.on_open_url(|url| {
        let _ = open::that(url.as_str());
    });
    // command palette: filter in Rust (Slint strings can't substring-match),
    // dispatch by re-invoking the existing callbacks.
    {
        let ui_w = ui.as_weak();
        ui.on_palette_filter(move |q| {
            let ui = ui_w.unwrap();
            let q = q.to_lowercase();
            let items: Vec<PaletteItem> = COMMANDS
                .iter()
                .filter(|(_, label)| label.to_lowercase().contains(&q))
                .map(|(id, label)| PaletteItem { id: (*id).into(), label: (*label).into() })
                .collect();
            ui.set_palette_items(ModelRc::new(VecModel::from(items)));
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_palette_run(move |id| {
            let ui = ui_w.unwrap();
            ui.set_palette_open(false);
            let key = ui.get_selected_key();
            match id.as_str() {
                "new" => ui.invoke_new_issue(),
                "refresh" => ui.invoke_refresh(),
                "edit" if !key.is_empty() => ui.invoke_edit_issue(key),
                "open" if !key.is_empty() => ui.invoke_open_browser(key),
                "account" => ui.invoke_show_login(),
                "quit" => ui.invoke_quit(),
                _ => {}
            }
        });
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
        pfields.push(PField { key: fkey, field: f, value: default_id, touched: false });
    }
    PENDING.with(|p| *p.borrow_mut() = Some(Pending { kind: PendingKind::Transition { tid }, key: key.clone(), fields: pfields }));
    ui.set_dialog_title(format!("Confirm: {key}").into());
    ui.set_dialog_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_dialog_open(true);
}

/// Populate and open the edit dialog from the issue's editmeta. Shows a curated
/// set (summary / priority / due date) plus any custom fields, prefilled where we
/// have the current value; only touched fields are submitted.
fn open_edit_dialog(ui: &AppWindow, key: String, metas: Vec<(String, TransitionField)>, issue: &Issue, users: &[User]) {
    let mut pfields = Vec::new();
    let mut rows: Vec<FieldRow> = Vec::new();
    for (fkey, mut f) in metas {
        let show = matches!(fkey.as_str(), "summary" | "assignee" | "priority" | "duedate" | "labels") || fkey.starts_with("customfield_");
        if !show {
            continue;
        }
        // The assignee field has no allowedValues from Jira; synthesize a pick-list
        // from the assignable users (id = accountId on Cloud, name on Server/DC).
        if fkey == "assignee" {
            f.allowed_values = users
                .iter()
                .map(|u| AllowedValue {
                    id: if u.account_id.is_empty() { u.name.clone() } else { u.account_id.clone() },
                    name: u.display_name.clone(),
                    value: String::new(),
                })
                .collect();
        }
        let is_select = f.has_options();
        let options: Vec<SharedString> = f.allowed_values.iter().map(|a| a.label().into()).collect();
        let (init_id, init_label) = prefill_field(&fkey, &f, issue, is_select);
        rows.push(FieldRow {
            name: f.name.clone().into(),
            options: ModelRc::new(VecModel::from(options)),
            is_select,
            value: init_label.into(),
        });
        pfields.push(PField { key: fkey, field: f, value: init_id, touched: false });
    }
    if pfields.is_empty() {
        ui.set_error("No editable fields for this issue.".into());
        return;
    }
    PENDING.with(|p| *p.borrow_mut() = Some(Pending { kind: PendingKind::Edit, key: key.clone(), fields: pfields }));
    ui.set_dialog_title(format!("Edit {key}").into());
    ui.set_dialog_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_dialog_open(true);
}

/// Build and open the "New issue" dialog from the available project/type targets.
fn open_create_dialog(ui: &AppWindow, targets: Vec<(String, String, String)>) {
    if targets.is_empty() {
        ui.set_error("No projects available to create issues in.".into());
        return;
    }
    // one pick-list entry per (project, type), id encodes "PROJKEY|Type"
    let combos: Vec<AllowedValue> = targets
        .iter()
        .map(|(pk, pn, ty)| AllowedValue { id: format!("{pk}|{ty}"), name: format!("{pn} ({pk}) — {ty}"), value: String::new() })
        .collect();
    let labels: Vec<SharedString> = combos.iter().map(|a| a.label().into()).collect();
    let first_label = combos.first().map(|a| a.label().to_string()).unwrap_or_default();
    let first_id = combos.first().map(|a| a.id.clone()).unwrap_or_default();
    let empty = || ModelRc::new(VecModel::from(Vec::<SharedString>::new()));

    let rows = vec![
        FieldRow { name: "Project / Type".into(), options: ModelRc::new(VecModel::from(labels)), is_select: true, value: first_label.into() },
        FieldRow { name: "Summary".into(), options: empty(), is_select: false, value: "".into() },
        FieldRow { name: "Description".into(), options: empty(), is_select: false, value: "".into() },
    ];
    let pfields = vec![
        PField { key: "__projtype".into(), field: TransitionField { name: "Project / Type".into(), allowed_values: combos, ..Default::default() }, value: first_id, touched: true },
        PField { key: "summary".into(), field: TransitionField::default(), value: String::new(), touched: false },
        PField { key: "description".into(), field: TransitionField::default(), value: String::new(), touched: false },
    ];
    PENDING.with(|p| *p.borrow_mut() = Some(Pending { kind: PendingKind::Create, key: String::new(), fields: pfields }));
    ui.set_dialog_title("New issue".into());
    ui.set_dialog_fields(ModelRc::new(VecModel::from(rows)));
    ui.set_dialog_open(true);
}

/// Prefill an edit field from the issue's current values where we can.
fn prefill_field(fkey: &str, f: &TransitionField, issue: &Issue, is_select: bool) -> (String, String) {
    match fkey {
        "summary" => (issue.summary().to_string(), issue.summary().to_string()),
        "duedate" => {
            let d = issue.fields.duedate.clone().unwrap_or_default();
            (d.clone(), d)
        }
        "labels" => {
            let s = issue.fields.labels.join(", ");
            (s.clone(), s)
        }
        "priority" if is_select => f
            .allowed_values
            .iter()
            .find(|a| a.label() == issue.priority())
            .or_else(|| f.allowed_values.first())
            .map(|a| (a.id.clone(), a.label().to_string()))
            .unwrap_or_default(),
        "assignee" if is_select => f
            .allowed_values
            .iter()
            .find(|a| a.label() == issue.assignee())
            .or_else(|| f.allowed_values.first())
            .map(|a| (a.id.clone(), a.label().to_string()))
            .unwrap_or_default(),
        _ if is_select => f.allowed_values.first().map(|a| (a.id.clone(), a.label().to_string())).unwrap_or_default(),
        _ => (String::new(), String::new()),
    }
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
