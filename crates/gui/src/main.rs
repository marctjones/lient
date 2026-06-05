// Lient — native Slint Jira daily-driver over lient-core.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use lient_core::client::transition_field_json;
use lient_core::model::{AllowedValue, Comment, CreateOption, Issue, Transition, TransitionField, User};
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
    /// Cached create options (project × type + required fields) for the New-issue
    /// dialog, so changing the project/type rebuilds its required fields instantly.
    static CREATE_OPTS: RefCell<Vec<CreateOption>> = const { RefCell::new(Vec::new()) };
    /// Result of an OAuth login, handed from the worker thread to the UI thread
    /// (the worker can't touch the Rc-held client cell).
    static PENDING_OAUTH: RefCell<Option<JiraConfig>> = const { RefCell::new(None) };
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
        let ui_w = ui.as_weak();
        ui.on_field_changed(move |i, value| {
            // returns (combo_id, summary, desc) when a Create-mode project/type
            // change requires rebuilding the dialog's required fields.
            let rebuild = PENDING.with(|p| {
                let mut pend = p.borrow_mut();
                let Some(pend) = pend.as_mut() else { return None };
                if let Some(pf) = pend.fields.get_mut(i as usize) {
                    pf.value = if pf.field.has_options() {
                        pf.field.allowed_values.iter().find(|a| a.label() == value.as_str()).map(|a| a.id.clone()).unwrap_or_else(|| value.to_string())
                    } else {
                        value.to_string()
                    };
                    pf.touched = true;
                }
                if matches!(pend.kind, PendingKind::Create) && pend.fields.get(i as usize).map(|f| f.key.as_str()) == Some("__projtype") {
                    let combo = pend.fields[i as usize].value.clone();
                    let get = |k: &str| pend.fields.iter().find(|f| f.key == k).map(|f| f.value.clone()).unwrap_or_default();
                    Some((combo, get("summary"), get("description")))
                } else {
                    None
                }
            });
            if let Some((combo, summary, desc)) = rebuild {
                build_create_dialog(&ui_w.unwrap(), &combo, &summary, &desc);
            }
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
                    let mut extra = serde_json::Map::new();
                    for pf in &pend.fields {
                        match pf.key.as_str() {
                            "__projtype" => {
                                let mut parts = pf.value.splitn(2, '|');
                                project = parts.next().unwrap_or("").to_string();
                                itype = parts.next().unwrap_or("").to_string();
                            }
                            "summary" => summary = pf.value.clone(),
                            "description" => desc = pf.value.clone(),
                            // a required create field
                            other if !pf.value.is_empty() => {
                                extra.insert(other.to_string(), transition_field_json(&pf.field, &pf.value));
                            }
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
                            j.create_issue(&project, &itype, &summary, d, serde_json::Value::Object(extra))
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
                    let users = j.assignable_users(&k).unwrap_or_default(); // assignee picker
                    let raw = j.raw_fields(&k).unwrap_or_default(); // current values to prefill
                    Ok((k, metas, users, raw))
                },
                |ui, (k, metas, users, raw)| open_edit_dialog(&ui, k, metas, &users, &raw),
            );
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_new_issue(move || {
            let ui = ui_w.unwrap();
            let jira = cell.borrow().clone();
            run(&ui, jira, |j| j.create_targets(), |ui, opts| {
                CREATE_OPTS.with(|c| *c.borrow_mut() = opts);
                build_create_dialog(&ui, "", "", "");
            });
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
    {
        let ui_w = ui.as_weak();
        ui.on_oauth_login(move || {
            let ui = ui_w.unwrap();
            let client = ui.get_login_oauth_client().trim().to_string();
            if client.is_empty() {
                ui.set_test_status("✗ Enter your OAuth client id".into());
                return;
            }
            let secret = ui.get_login_oauth_secret().trim().to_string();
            ui.set_testing(true);
            ui.set_test_status("opening browser — authorize in Atlassian…".into());
            let w = ui.as_weak();
            // The worker can't hold the Rc client cell, so on success it stashes the
            // config in a thread-local and fires `oauth-finished` (handled below).
            std::thread::spawn(move || {
                let res = lient_core::oauth::login(&client, if secret.is_empty() { None } else { Some(&secret) });
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = w.upgrade() else { return };
                    ui.set_testing(false);
                    match res {
                        Ok(cfg) => {
                            PENDING_OAUTH.with(|p| *p.borrow_mut() = Some(cfg));
                            ui.invoke_oauth_finished();
                        }
                        Err(e) => ui.set_test_status(format!("✗ {e:#}").into()),
                    }
                });
            });
        });
    }
    {
        let (ui_w, cell) = (ui.as_weak(), cell.clone());
        ui.on_oauth_finished(move || {
            let ui = ui_w.unwrap();
            if let Some(cfg) = PENDING_OAUTH.with(|p| p.borrow_mut().take()) {
                let _ = cfg.save();
                *cell.borrow_mut() = Arc::new(JiraClient::new(cfg));
                ui.set_demo(false);
                ui.set_test_status("".into());
                ui.set_view("main".into());
                let jira = cell.borrow().clone();
                load_me(&ui, jira.clone());
                load_list(&ui, jira);
            }
        });
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
            ui.set_palette_selected(0);
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
        // show the cached My Work list instantly, then refresh from the network
        if !demo {
            let cached = cache_load();
            if !cached.is_empty() {
                ui.set_issues(ModelRc::new(VecModel::from(cached.iter().map(to_row).collect::<Vec<_>>())));
            }
        }
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
    let inbox = ui.get_inbox_mode();
    run(
        ui,
        jira,
        move |j| {
            if inbox {
                // "needs attention": assigned to me, changed recently, freshest first
                Ok(j.search("assignee = currentUser() AND updated >= -7d ORDER BY updated DESC", 100)?.issues)
            } else {
                j.my_issues(100)
            }
        },
        |ui, issues| {
            ui.set_loading(false);
            ui.set_error("".into());
            let rows: Vec<IssueRow> = issues.iter().map(to_row).collect();
            ui.set_issues(ModelRc::new(VecModel::from(rows)));
            // cache the My Work list for instant/offline open next time
            if !ui.get_inbox_mode() {
                cache_save(&issues);
            }
        },
    );
}

fn load_detail(ui: &AppWindow, jira: Arc<dyn Jira>, key: String) {
    // show the cached detail instantly (and keep it if the network is down)
    if !ui.get_demo() {
        if let Some(cached) = detail_cache_load(&key) {
            apply_detail(ui, &cached, &[]);
        }
    }
    let demo = ui.get_demo();
    run(
        ui,
        jira,
        move |j| {
            let issue = j.issue(&key)?;
            let trans = j.transitions(&key).unwrap_or_default();
            Ok((issue, trans))
        },
        move |ui, (issue, trans)| {
            if !demo {
                detail_cache_save(&issue);
            }
            apply_detail(&ui, &issue, &trans);
        },
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
fn open_edit_dialog(ui: &AppWindow, key: String, metas: Vec<(String, TransitionField)>, users: &[User], raw: &serde_json::Map<String, serde_json::Value>) {
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
        let (init_id, init_label) = prefill_from_raw(&fkey, &f, raw, is_select);
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

/// Build (or rebuild) the "New issue" dialog from the cached create options for
/// the given project/type combo, preserving the typed summary/description. Adds
/// the selected combo's required fields as editable rows.
fn build_create_dialog(ui: &AppWindow, selected_id: &str, summary: &str, description: &str) {
    CREATE_OPTS.with(|opts| {
        let opts = opts.borrow();
        if opts.is_empty() {
            ui.set_error("No projects available to create issues in.".into());
            return;
        }
        let combo_id = |o: &CreateOption| format!("{}|{}", o.project_key, o.type_name);
        let combos: Vec<AllowedValue> = opts
            .iter()
            .map(|o| AllowedValue { id: combo_id(o), name: format!("{} ({}) — {}", o.project_name, o.project_key, o.type_name), value: String::new() })
            .collect();
        let sel_id = if selected_id.is_empty() { combos.first().map(|a| a.id.clone()).unwrap_or_default() } else { selected_id.to_string() };
        let sel_label = combos.iter().find(|a| a.id == sel_id).map(|a| a.label().to_string()).unwrap_or_default();
        let empty = || ModelRc::new(VecModel::from(Vec::<SharedString>::new()));

        let mut rows: Vec<FieldRow> = Vec::new();
        let mut pfields: Vec<PField> = Vec::new();

        let labels: Vec<SharedString> = combos.iter().map(|a| a.label().into()).collect();
        rows.push(FieldRow { name: "Project / Type".into(), options: ModelRc::new(VecModel::from(labels)), is_select: true, value: sel_label.into() });
        pfields.push(PField { key: "__projtype".into(), field: TransitionField { name: "Project / Type".into(), allowed_values: combos.clone(), ..Default::default() }, value: sel_id.clone(), touched: true });

        rows.push(FieldRow { name: "Summary".into(), options: empty(), is_select: false, value: summary.into() });
        pfields.push(PField { key: "summary".into(), field: TransitionField::default(), value: summary.to_string(), touched: false });
        rows.push(FieldRow { name: "Description".into(), options: empty(), is_select: false, value: description.into() });
        pfields.push(PField { key: "description".into(), field: TransitionField::default(), value: description.to_string(), touched: false });

        // the selected combo's required fields
        if let Some(o) = opts.iter().find(|o| combo_id(o) == sel_id) {
            for (fkey, f) in &o.required {
                let is_select = f.has_options();
                let opt_labels: Vec<SharedString> = f.allowed_values.iter().map(|a| a.label().into()).collect();
                let (init_id, init_label) = if is_select {
                    f.allowed_values.first().map(|a| (a.id.clone(), a.label().to_string())).unwrap_or_default()
                } else {
                    (String::new(), String::new())
                };
                rows.push(FieldRow { name: format!("{} *", f.name).into(), options: ModelRc::new(VecModel::from(opt_labels)), is_select, value: init_label.into() });
                pfields.push(PField { key: fkey.clone(), field: f.clone(), value: init_id, touched: false });
            }
        }

        PENDING.with(|p| *p.borrow_mut() = Some(Pending { kind: PendingKind::Create, key: String::new(), fields: pfields }));
        ui.set_dialog_title("New issue".into());
        ui.set_dialog_fields(ModelRc::new(VecModel::from(rows)));
        ui.set_dialog_open(true);
    });
}

/// Prefill an edit field from the issue's raw current values. Returns
/// (submit_value, display_label). For pick-lists the submit value is the
/// allowedValue **id**; for text it's the value itself.
fn prefill_from_raw(fkey: &str, f: &TransitionField, raw: &serde_json::Map<String, serde_json::Value>, is_select: bool) -> (String, String) {
    let val = raw.get(fkey);
    if is_select {
        // current id: object {"id"|"accountId"|"name": ...} (arrays → first)
        let cur = val
            .map(|v| if v.is_array() { v.as_array().and_then(|a| a.first()).unwrap_or(&serde_json::Value::Null) } else { v })
            .and_then(|v| v.get("id").or_else(|| v.get("accountId")).or_else(|| v.get("name")))
            .and_then(|x| x.as_str());
        if let Some(id) = cur {
            if let Some(av) = f.allowed_values.iter().find(|a| a.id == id) {
                return (av.id.clone(), av.label().to_string());
            }
        }
        // no current value → default to the first option
        return f.allowed_values.first().map(|a| (a.id.clone(), a.label().to_string())).unwrap_or_default();
    }
    if fkey == "labels" {
        let s = val
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return (s.clone(), s);
    }
    let s = val.and_then(|v| v.as_str()).map(String::from).unwrap_or_default();
    (s.clone(), s)
}

// ---- local cache (instant open + offline read of the My Work list) ----------

fn cache_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("lient").join("my-work.json"))
}
fn cache_save(issues: &[Issue]) {
    if let Some(p) = cache_path() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(j) = serde_json::to_string(issues) {
            let _ = std::fs::write(p, j);
        }
    }
}
fn cache_load() -> Vec<Issue> {
    cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Vec<Issue>>(&s).ok())
        .unwrap_or_default()
}

fn detail_cache_path(key: &str) -> Option<std::path::PathBuf> {
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    dirs::cache_dir().map(|d| d.join("lient").join("details").join(format!("{safe}.json")))
}
fn detail_cache_save(issue: &Issue) {
    if let Some(p) = detail_cache_path(&issue.key) {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(j) = serde_json::to_string(issue) {
            let _ = std::fs::write(p, j);
        }
    }
}
fn detail_cache_load(key: &str) -> Option<Issue> {
    let s = std::fs::read_to_string(detail_cache_path(key)?).ok()?;
    serde_json::from_str(&s).ok()
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

#[cfg(test)]
mod ui_tests {
    //! Headless GUI tests: construct the real `AppWindow` via Slint's testing
    //! backend and assert the data-binding helpers + pure logic. Locks the
    //! conversion/binding behavior that the rest of the app relies on.
    use super::*;
    use slint::Model;

    fn sample_issue() -> Issue {
        serde_json::from_str(
            r#"{"key":"X-1","fields":{"summary":"Hi there","status":{"name":"To Do"},
                "priority":{"name":"High"},"issuetype":{"name":"Task"},
                "assignee":{"displayName":"Sam"},"labels":["urgent"]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn window_constructs_binds_detail_and_list() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let issue = sample_issue();

        // row conversion
        let row = to_row(&issue);
        assert_eq!(row.key, "X-1");
        assert_eq!(row.status, "To Do");
        assert_eq!(row.assignee, "Sam");

        // list model populates the window
        ui.set_issues(slint::ModelRc::new(slint::VecModel::from(vec![to_row(&issue)])));
        assert_eq!(ui.get_issues().row_count(), 1);

        // detail binding sets the Slint properties
        apply_detail(&ui, &issue, &[]);
        assert_eq!(ui.get_d_summary().as_str(), "Hi there");
        assert_eq!(ui.get_d_status().as_str(), "To Do");
        let meta = ui.get_d_meta();
        assert!(meta.contains("High") && meta.contains("Sam") && meta.contains("urgent"));
    }

    #[test]
    fn palette_commands_filter() {
        let hits: Vec<&str> = COMMANDS
            .iter()
            .filter(|(_, l)| l.to_lowercase().contains("refresh"))
            .map(|(_, l)| *l)
            .collect();
        assert_eq!(hits.len(), 1);
        // every command id is dispatchable (matches the on_palette_run arms)
        for (id, _) in COMMANDS {
            assert!(matches!(*id, "new" | "refresh" | "edit" | "open" | "account" | "quit"));
        }
    }

    #[test]
    fn cache_roundtrips() {
        let issues = vec![sample_issue()];
        cache_save(&issues);
        let back = cache_load();
        assert_eq!(back.first().map(|i| i.key.as_str()), Some("X-1"));

        // detail cache roundtrips per key
        detail_cache_save(&sample_issue());
        let d = detail_cache_load("X-1").expect("cached detail");
        assert_eq!(d.summary(), "Hi there");
        assert!(detail_cache_load("NOPE-9").is_none());
    }
}
