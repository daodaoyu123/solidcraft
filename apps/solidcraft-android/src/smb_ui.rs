//! The file dialogs' locations: Android Files (the system pickers, cloud drives) or a saved
//! network (SMB) location, browsed in the app. Open copies the network file into the app's
//! storage and links the copy to it; Save, Save As and the exports write a local file that
//! [`crate::bridge::Links::sync`] uploads once it is written, as for a picked document.
//!
//! The network calls run on worker threads; each sends its answer back and wakes the window.
//! Connections are kept in `<data>/smb/connections.json` without passwords: those are in the
//! Android Keystore-backed store (MainActivity `smbSecret*`), never logged or written in plain.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::bridge;
use crate::smb_core::{self as core, Conn, Entry, SmbError};

/// What a dialog was asked for.
pub enum Kind {
    Open,
    /// Save under this suggested name (with its extension).
    Save(String),
    /// Show in folder: a connection id and a folder in it.
    Show(String, String),
}

pub struct Request {
    pub id: i32,
    pub kind: Kind,
}

static REQUESTS: Mutex<Vec<Request>> = Mutex::new(Vec::new());
static CONNS: Mutex<Vec<Conn>> = Mutex::new(Vec::new());
static DATA: Mutex<Option<PathBuf>> = Mutex::new(None);
static PASSWORD_FIELD: &str = "smb-password";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// At start: where the app keeps its files; loads the saved connections.
pub fn init(data: &Path) {
    *lock(&DATA) = Some(data.to_path_buf());
    let path = data.join("smb").join("connections.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<Vec<Conn>>(&text) {
            Ok(c) => *lock(&CONNS) = c,
            Err(e) => log::warn!("{}: {e}", path.display()),
        }
    }
}

fn save_conns() -> Result<(), String> {
    let Some(data) = lock(&DATA).clone() else { return Err("no app storage".into()) };
    let dir = data.join("smb");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&*lock(&CONNS)).map_err(|e| e.to_string())?;
    let tmp = dir.join("connections.json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("connections.json")).map_err(|e| e.to_string())
}

/// A saved connection by id.
pub fn conn(id: &str) -> Option<Conn> {
    lock(&CONNS).iter().find(|c| c.id == id).cloned()
}

/// The stored password of a connection (asked by the SMB layer only when it signs in).
pub fn stored_password(id: String) -> impl Fn() -> Result<Option<String>, String> {
    move || bridge::smb_secret_get(&id)
}

/// Ask for a dialog (from the app's picker calls and Show in folder, any thread).
pub fn request(r: Request) {
    lock(&REQUESTS).push(r);
    if let Some(ctx) = bridge::EGUI.get() {
        ctx.request_repaint();
    }
}

/// Whether the focused field is the password field (the keyboard then hides what is typed and
/// learns nothing).
pub fn is_password_field(focus: Option<egui::Id>) -> bool {
    focus == Some(egui::Id::new(PASSWORD_FIELD))
}

/// A new folder for a local copy: `<data>/documents/<fresh>/` (as MainActivity's freshDir).
pub fn fresh_dir() -> Result<PathBuf, String> {
    let Some(data) = lock(&DATA).clone() else { return Err("no app storage".into()) };
    let root = data.join("documents");
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    for n in 0.. {
        let d = root.join(format!("{}-{n}", base36(ms)));
        if !d.exists() && std::fs::create_dir_all(&d).is_ok() {
            return Ok(d);
        }
        if n > 1000 {
            break;
        }
    }
    Err("could not make a folder in the app's storage".into())
}

fn base36(mut v: u128) -> String {
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut s = Vec::new();
    loop {
        s.push(D[(v % 36) as usize]);
        v /= 36;
        if v == 0 {
            break;
        }
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn sanitize(n: &str) -> String {
    n.trim().replace(['/', '\\', ':'], "_")
}

fn new_conn_id() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut id = format!("c{}", base36(ms));
    while conn(&id).is_some() {
        id.push('x');
    }
    id
}

fn size_text(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

enum Msg {
    Listed(u64, String, Result<Vec<Entry>, SmbError>),
    Tested(u64, Result<String, String>),
    Downloaded(u64, Result<(PathBuf, String), String>),
    Made(u64, String, Result<(), SmbError>),
}

struct Form {
    /// The connection being edited (`None`: a new one).
    editing: bool,
    conn: Conn,
    password: String,
    has_password: bool,
    note: Option<(bool, String)>,
    testing: bool,
}

struct Browse {
    conn: Conn,
    dir: String,
    entries: Option<Result<Vec<Entry>, String>>,
    /// The name to save as (Save).
    name: String,
    new_folder: Option<String>,
    /// Asked to replace a file of the same name.
    confirm: bool,
}

enum Page {
    Choose,
    Form(Form),
    Browse(Browse),
}

struct Active {
    id: i32,
    kind: Kind,
    page: Page,
    error: Option<String>,
    busy: Option<String>,
    token: u64,
}

/// The location dialog, drawn over the app while a picker request is open.
pub struct Chooser {
    active: Option<Active>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    token: u64,
}

impl Default for Chooser {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { active: None, tx, rx, token: 0 }
    }
}

const ROW: f32 = 40.0;

fn spawn(tx: &Sender<Msg>, ctx: &egui::Context, f: impl FnOnce() -> Msg + Send + 'static) {
    let tx = tx.clone();
    let ctx = ctx.clone();
    let r = std::thread::Builder::new().name("smb".into()).spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    if let Err(e) = r {
        log::error!("SMB worker: {e}");
    }
}

fn list_job(tx: &Sender<Msg>, ctx: &egui::Context, token: u64, c: &Conn, dir: &str) {
    let c = c.clone();
    let dir = dir.to_owned();
    spawn(tx, ctx, move || {
        let pw = stored_password(c.id.clone());
        let r = core::list(&c, &pw, &dir);
        Msg::Listed(token, dir, r)
    });
}

impl Chooser {
    /// The egui form (legacy); add/edit now uses the Java dialog.
    pub fn form_wants_keyboard(&self) -> bool {
        false
    }

    fn conn_form_json(c: &Conn, editing: bool) -> String {
        serde_json::json!({
            "id": c.id,
            "name": c.name,
            "host": c.host,
            "share": c.share,
            "user": c.user,
            "domain": c.domain,
            "editing": editing,
        })
        .to_string()
    }

    fn apply_java_conn(&mut self, js: &str) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_str(js).map_err(|e| e.to_string())?;
        let id = v.get("id").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).ok_or("missing connection id")?;
        let mut c = Conn {
            id: id.to_owned(),
            name: v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_owned(),
            host: v.get("host").and_then(|x| x.as_str()).unwrap_or("").trim().trim_start_matches("\\\\").trim_start_matches("smb://").trim_end_matches('/').to_owned(),
            share: v.get("share").and_then(|x| x.as_str()).unwrap_or("").trim().trim_matches(['/', '\\']).to_owned(),
            user: v.get("user").and_then(|x| x.as_str()).unwrap_or("").trim().to_owned(),
            domain: v.get("domain").and_then(|x| x.as_str()).unwrap_or("").trim().to_owned(),
        };
        if c.host.is_empty() || c.share.is_empty() {
            return Err("Enter the host and the share.".into());
        }
        let password = v.get("password").and_then(|x| x.as_str()).unwrap_or("");
        let stored = if c.guest() {
            bridge::smb_secret_set(&c.id, None)
        } else if !password.is_empty() {
            bridge::smb_secret_set(&c.id, Some(password))
        } else {
            Ok(())
        };
        stored?;
        {
            let mut all = lock(&CONNS);
            match all.iter_mut().find(|x| x.id == c.id) {
                Some(x) => *x = c,
                None => all.push(c),
            }
        }
        save_conns().map_err(|e| format!("Couldn't save the connection: {e}"))?;
        if let Some(a) = &mut self.active {
            a.page = Page::Choose;
        }
        Ok(())
    }

    fn answer(&mut self, value: Option<String>) {
        if let Some(a) = self.active.take()
            && !matches!(a.kind, Kind::Show(..))
        {
            bridge::answer(a.id, value);
        }
    }

    fn browse(&mut self, ctx: &egui::Context, c: Conn, dir: String) {
        let Some(a) = &mut self.active else { return };
        let name = match &a.kind {
            Kind::Save(n) => n.clone(),
            _ => String::new(),
        };
        a.error = None;
        list_job(&self.tx, ctx, a.token, &c, &dir);
        a.page = Page::Browse(Browse { conn: c, dir, entries: None, name, new_folder: None, confirm: false });
    }

    fn take_requests(&mut self, ctx: &egui::Context) {
        let reqs: Vec<Request> = std::mem::take(&mut *lock(&REQUESTS));
        for r in reqs {
            if self.active.is_some() {
                // One dialog at a time; the app asks for one at a time anyway.
                if !matches!(r.kind, Kind::Show(..)) {
                    bridge::answer(r.id, None);
                }
                continue;
            }
            self.token += 1;
            let show = match &r.kind {
                Kind::Show(cid, dir) => Some((cid.clone(), dir.clone())),
                _ => None,
            };
            self.active = Some(Active { id: r.id, kind: r.kind, page: Page::Choose, error: None, busy: None, token: self.token });
            if let Some((cid, dir)) = show {
                match conn(&cid) {
                    Some(c) => self.browse(ctx, c, dir),
                    None => {
                        if let Some(a) = &mut self.active {
                            a.error = Some("That network location was removed.".into());
                        }
                    }
                }
            }
        }
    }

    fn take_messages(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            let Some(a) = &mut self.active else { continue };
            match m {
                Msg::Listed(t, dir, r) if t == a.token => {
                    if let Page::Browse(b) = &mut a.page
                        && b.dir == dir
                    {
                        b.entries = Some(r.map_err(|e| e.to_string()));
                    }
                }
                Msg::Tested(t, r) if t == a.token => {
                    if let Page::Form(f) = &mut a.page {
                        f.testing = false;
                        f.note = Some(match r {
                            Ok(s) => (true, s),
                            Err(s) => (false, s),
                        });
                    }
                }
                Msg::Downloaded(t, r) if t == a.token => {
                    a.busy = None;
                    match r {
                        Ok((local, uri)) => {
                            let l = local.to_string_lossy().into_owned();
                            bridge::link(&l, &uri, true);
                            self.answer(Some(l));
                        }
                        Err(e) => a.error = Some(e),
                    }
                }
                Msg::Made(t, dir, r) if t == a.token => {
                    a.busy = None;
                    match r {
                        Ok(()) => {
                            if let Page::Browse(b) = &a.page {
                                let c = b.conn.clone();
                                self.browse(ctx, c, dir);
                            }
                        }
                        Err(e) => a.error = Some(e.to_string()),
                    }
                }
                _ => {}
            }
        }
    }

    /// Each frame: take new requests and finished network calls, and draw the dialog.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.take_requests(ctx);
        self.take_messages(ctx);
        for (t, ans) in bridge::take_smb_conn_answers() {
            let apply = self.active.as_ref().filter(|a| a.token as i32 == t).map(|_| ans);
            if let Some(Some(js)) = apply {
                let result = self.apply_java_conn(&js);
                if let Some(a) = &mut self.active {
                    if a.token as i32 == t {
                        a.error = result.err();
                    }
                }
            }
        }
        if self.active.is_none() {
            return;
        }
        let width = (ctx.content_rect().width() - 32.0).clamp(240.0, 560.0);
        let max_list = (ctx.content_rect().height() * 0.45).max(160.0);
        let id = egui::Id::new("smb-chooser");
        let mut action = None;
        egui::Modal::new(id).area(egui::Modal::default_area(id).fade_in(false)).show(ctx, |ui| {
            ui.set_width(width);
            if let Some(a) = &mut self.active {
                action = draw(ui, a, max_list);
            }
        });
        if let Some(act) = action {
            self.act(ctx, act);
        }
        if self.active.as_ref().is_some_and(|a| a.busy.is_some()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn act(&mut self, ctx: &egui::Context, act: Action) {
        match act {
            Action::Cancel => self.answer(None),
            Action::Files => {
                if let Some(a) = self.active.take() {
                    match &a.kind {
                        Kind::Open => bridge::pick_open_files(a.id),
                        Kind::Save(n) => bridge::pick_save_files(a.id, n),
                        Kind::Show(..) => {}
                    }
                }
            }
            Action::Back => {
                if let Some(a) = &mut self.active {
                    a.page = Page::Choose;
                    a.error = None;
                }
            }
            Action::Add => {
                if let Some(a) = &mut self.active {
                    a.error = None;
                    let c = Conn { id: new_conn_id(), ..Default::default() };
                    bridge::show_smb_connection_form(a.token as i32, &Self::conn_form_json(&c, false));
                }
            }
            Action::Edit(cid) => {
                if let (Some(a), Some(c)) = (&mut self.active, conn(&cid)) {
                    a.error = None;
                    bridge::show_smb_connection_form(a.token as i32, &Self::conn_form_json(&c, true));
                }
            }
            Action::Browse(cid, dir) => {
                if let Some(c) = conn(&cid) {
                    self.browse(ctx, c, dir);
                }
            }
            Action::Test => {
                let Some(a) = &mut self.active else { return };
                let Page::Form(f) = &mut a.page else { return };
                f.testing = true;
                f.note = None;
                let c = f.conn.clone();
                // The typed password when there is one, else the saved one; never kept past the test.
                let typed = (!f.password.is_empty()).then(|| f.password.clone());
                let token = a.token;
                spawn(&self.tx, ctx, move || {
                    let r = (|| {
                        let dialect = core::probe(&c.host).map_err(|e| e.to_string())?;
                        let reached = format!("Reached {} ({dialect}).", c.host.trim());
                        if c.share.trim().is_empty() {
                            return Ok(format!("{reached} Enter the share to sign in."));
                        }
                        core::forget(&c.id);
                        let id = c.id.clone();
                        let pw = move || match &typed {
                            Some(p) => Ok(Some(p.clone())),
                            None => bridge::smb_secret_get(&id),
                        };
                        let n = core::check(&c, &pw).map_err(|e| format!("{reached} {e}"))?;
                        core::forget(&c.id);
                        let who = if c.guest() { "as guest".to_owned() } else { format!("as {}", c.user.trim()) };
                        Ok(format!("{reached} Signed in {who}: {} has {n} items at the top.", c.share.trim()))
                    })();
                    Msg::Tested(token, r)
                });
            }
            Action::SaveConn => {
                let Some(a) = &mut self.active else { return };
                let Page::Form(f) = &mut a.page else { return };
                let mut c = f.conn.clone();
                c.host = c.host.trim().trim_start_matches("\\\\").trim_start_matches("smb://").trim_end_matches('/').to_owned();
                c.share = c.share.trim().trim_matches(['/', '\\']).to_owned();
                c.user = c.user.trim().to_owned();
                c.domain = c.domain.trim().to_owned();
                c.name = c.name.trim().to_owned();
                if c.host.is_empty() || c.share.is_empty() {
                    f.note = Some((false, "Enter the host and the share.".into()));
                    return;
                }
                let stored = if c.guest() {
                    bridge::smb_secret_set(&c.id, None)
                } else if !f.password.is_empty() {
                    bridge::smb_secret_set(&c.id, Some(&f.password))
                } else {
                    Ok(())
                };
                f.password.clear();
                if let Err(e) = stored {
                    f.note = Some((false, e));
                    return;
                }
                {
                    let mut all = lock(&CONNS);
                    match all.iter_mut().find(|x| x.id == c.id) {
                        Some(x) => *x = c.clone(),
                        None => all.push(c.clone()),
                    }
                }
                core::forget(&c.id);
                if let Err(e) = save_conns() {
                    f.note = Some((false, format!("Couldn't save the connection: {e}")));
                    return;
                }
                a.page = Page::Choose;
            }
            Action::DeleteConn(cid) => {
                lock(&CONNS).retain(|c| c.id != cid);
                let _ = bridge::smb_secret_set(&cid, None);
                core::forget(&cid);
                let r = save_conns();
                if let Some(a) = &mut self.active {
                    a.page = Page::Choose;
                    a.error = r.err();
                }
            }
            Action::OpenFile(name) => {
                let Some(a) = &mut self.active else { return };
                let Page::Browse(b) = &a.page else { return };
                let c = b.conn.clone();
                let remote = core::join(&b.dir, &name);
                a.busy = Some(format!("Downloading {name}…"));
                a.error = None;
                let token = a.token;
                spawn(&self.tx, ctx, move || {
                    let r = (|| {
                        let local = fresh_dir()?.join(sanitize(&name));
                        let pw = stored_password(c.id.clone());
                        core::download(&c, &pw, &remote, &local).map_err(|e| e.to_string())?;
                        Ok((local, core::uri(&c.id, &remote, false)))
                    })();
                    Msg::Downloaded(token, r)
                });
            }
            Action::SaveHere => {
                let Some(a) = &mut self.active else { return };
                let Kind::Save(suggested) = &a.kind else { return };
                let Page::Browse(b) = &mut a.page else { return };
                let mut name = sanitize(&b.name);
                if let Some(ext) = Path::new(suggested).extension().map(|e| e.to_string_lossy().into_owned())
                    && !name.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase()))
                {
                    name = format!("{name}.{ext}");
                }
                if name.is_empty() || name.starts_with('.') {
                    a.error = Some("Enter a name.".into());
                    return;
                }
                let urdf = name.to_lowercase().ends_with(".urdf");
                let exists = b.entries.as_ref().and_then(|e| e.as_ref().ok()).is_some_and(|e| e.iter().any(|x| !x.dir && x.name.eq_ignore_ascii_case(&name)));
                if exists && !b.confirm && !urdf {
                    b.confirm = true;
                    b.name = name;
                    return;
                }
                let local = match fresh_dir() {
                    Ok(d) => d.join(&name),
                    Err(e) => {
                        a.error = Some(e);
                        return;
                    }
                };
                // A URDF writes its meshes beside it: the whole folder goes to this network folder.
                let uri = if urdf { core::uri(&b.conn.id, &b.dir, true) } else { core::uri(&b.conn.id, &core::join(&b.dir, &name), false) };
                let l = local.to_string_lossy().into_owned();
                bridge::link(&l, &uri, false);
                self.answer(Some(l));
            }
            Action::MakeFolder(name) => {
                let Some(a) = &mut self.active else { return };
                let Page::Browse(b) = &a.page else { return };
                let name = sanitize(&name);
                if name.is_empty() {
                    return;
                }
                let c = b.conn.clone();
                let dir = core::join(&b.dir, &name);
                a.busy = Some(format!("Making {name}…"));
                let token = a.token;
                spawn(&self.tx, ctx, move || {
                    let pw = stored_password(c.id.clone());
                    let r = core::mkdir(&c, &pw, &dir);
                    Msg::Made(token, dir, r)
                });
            }
        }
    }
}

enum Action {
    Cancel,
    Files,
    Back,
    Add,
    Edit(String),
    Browse(String, String),
    Test,
    SaveConn,
    DeleteConn(String),
    OpenFile(String),
    SaveHere,
    MakeFolder(String),
}

fn big(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(text).min_size(egui::vec2(0.0, ROW)))
}

fn wide(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let w = ui.available_width();
    ui.add(egui::Button::new(text).min_size(egui::vec2(w, ROW)))
}

fn draw(ui: &mut egui::Ui, a: &mut Active, max_list: f32) -> Option<Action> {
    let mut act = None;
    let saving = matches!(a.kind, Kind::Save(_));
    let showing = matches!(a.kind, Kind::Show(..));
    match &mut a.page {
        Page::Choose => {
            ui.heading(if saving { "Save to" } else { "Open from" });
            ui.add_space(6.0);
            if wide(ui, "Android Files (this device, Drive, other apps)").clicked() {
                act = Some(Action::Files);
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Network (SMB)").strong());
            let conns = lock(&CONNS).clone();
            if conns.is_empty() {
                ui.label("No network locations yet. Add one: a NAS (TrueNAS) or a shared folder on a PC.");
            }
            for c in conns {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 80.0).max(120.0);
                    let who = if c.guest() { "guest".to_owned() } else { c.user.clone() };
                    let text = format!("{}   ({}, {})", c.label(), c.host, who);
                    if ui.add(egui::Button::new(text).min_size(egui::vec2(w, ROW))).clicked() {
                        act = Some(Action::Browse(c.id.clone(), String::new()));
                    }
                    if big(ui, "Edit").clicked() {
                        act = Some(Action::Edit(c.id.clone()));
                    }
                });
            }
            if big(ui, "Add network location…").clicked() {
                act = Some(Action::Add);
            }
            ui.add_space(8.0);
            if big(ui, "Cancel").clicked() {
                act = Some(Action::Cancel);
            }
        }
        Page::Form(f) => {
            ui.heading(if f.editing { "Network location" } else { "Add network location" });
            ui.add_space(4.0);
            let fw = (ui.available_width() - 110.0).max(140.0);
            const HOST_FIELD: &str = "smb-host-field";
            if ui.memory(|m| m.focused()).is_none() {
                ui.memory_mut(|m| m.request_focus(egui::Id::new(HOST_FIELD)));
            }
            egui::Grid::new("smb-form").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut f.conn.name).hint_text("optional, e.g. TrueNAS").desired_width(fw));
                ui.end_row();
                ui.label("Host");
                let host = egui::TextEdit::singleline(&mut f.conn.host)
                    .id(egui::Id::new(HOST_FIELD))
                    .hint_text("IP address or name, e.g. 192.168.1.20")
                    .desired_width(fw);
                ui.add(host);
                ui.end_row();
                ui.label("Share");
                ui.add(egui::TextEdit::singleline(&mut f.conn.share).hint_text("the shared folder's name").desired_width(fw));
                ui.end_row();
                ui.label("User");
                ui.add(egui::TextEdit::singleline(&mut f.conn.user).hint_text("leave empty for guest").desired_width(fw));
                ui.end_row();
                ui.label("Domain");
                ui.add(egui::TextEdit::singleline(&mut f.conn.domain).hint_text("optional").desired_width(fw));
                ui.end_row();
                ui.label("Password");
                let hint = if f.has_password { "saved; leave empty to keep it" } else { "" };
                ui.add(egui::TextEdit::singleline(&mut f.password).password(true).id(egui::Id::new(PASSWORD_FIELD)).hint_text(hint).desired_width(fw));
                ui.end_row();
            });
            ui.label(egui::RichText::new("The password is kept encrypted by the Android Keystore on this device.").small().weak());
            if let Some((ok, note)) = &f.note {
                let color = if *ok { ui.visuals().text_color() } else { ui.visuals().error_fg_color };
                ui.label(egui::RichText::new(note.as_str()).color(color));
            }
            ui.horizontal_wrapped(|ui| {
                if f.testing {
                    ui.add(egui::Spinner::new());
                    ui.label("Testing…");
                } else if big(ui, "Test").clicked() {
                    act = Some(Action::Test);
                }
                if big(ui, "Save").clicked() {
                    act = Some(Action::SaveConn);
                }
                if f.editing && big(ui, "Delete").clicked() {
                    act = Some(Action::DeleteConn(f.conn.id.clone()));
                }
                if big(ui, "Back").clicked() {
                    act = Some(Action::Back);
                }
            });
        }
        Page::Browse(b) => {
            ui.heading(b.conn.label());
            let shown = if b.dir.is_empty() { format!("\\\\{}\\{}", b.conn.host, b.conn.share) } else { format!("\\\\{}\\{}\\{}", b.conn.host, b.conn.share, b.dir.replace('/', "\\")) };
            ui.label(egui::RichText::new(shown).weak());
            ui.horizontal_wrapped(|ui| {
                if !b.dir.is_empty() && big(ui, "⬆ Up").clicked() {
                    act = Some(Action::Browse(b.conn.id.clone(), core::parent(&b.dir)));
                }
                if big(ui, "Refresh").clicked() {
                    act = Some(Action::Browse(b.conn.id.clone(), b.dir.clone()));
                }
                if saving && b.new_folder.is_none() && big(ui, "New folder").clicked() {
                    b.new_folder = Some(String::new());
                }
            });
            if let Some(nf) = &mut b.new_folder {
                let mut make = false;
                let mut drop = false;
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(nf).hint_text("folder name").desired_width((ui.available_width() - 150.0).max(100.0)));
                    make = big(ui, "Make").clicked();
                    drop = big(ui, "✕").clicked();
                });
                if make {
                    act = Some(Action::MakeFolder(nf.clone()));
                }
                if make || drop {
                    b.new_folder = None;
                }
            }
            ui.separator();
            egui::ScrollArea::vertical().max_height(max_list).auto_shrink([false, true]).show(ui, |ui| match &b.entries {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label("Loading…");
                    });
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e.as_str()).color(ui.visuals().error_fg_color));
                    if big(ui, "Edit connection").clicked() {
                        act = Some(Action::Edit(b.conn.id.clone()));
                    }
                }
                Some(Ok(entries)) => {
                    if entries.is_empty() {
                        ui.label("This folder is empty.");
                    }
                    for e in entries {
                        let text = if e.dir { format!("📁 {}", e.name) } else { format!("{}    {}", e.name, size_text(e.size)) };
                        let w = ui.available_width();
                        let r = ui.add(egui::Button::new(text).frame(false).min_size(egui::vec2(w, ROW)));
                        if r.clicked() {
                            if e.dir {
                                act = Some(Action::Browse(b.conn.id.clone(), core::join(&b.dir, &e.name)));
                            } else if saving {
                                b.name = e.name.clone();
                                b.confirm = false;
                            } else if !showing {
                                act = Some(Action::OpenFile(e.name.clone()));
                            }
                        }
                    }
                }
            });
            ui.separator();
            if saving {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    if ui.add(egui::TextEdit::singleline(&mut b.name).desired_width((ui.available_width() - 130.0).max(100.0))).changed() {
                        b.confirm = false;
                    }
                    let label = if b.confirm { "Replace" } else { "Save here" };
                    if big(ui, label).clicked() {
                        act = Some(Action::SaveHere);
                    }
                });
                if b.confirm {
                    ui.label(egui::RichText::new(format!("{} is already here. Replace it?", b.name)).color(ui.visuals().warn_fg_color));
                }
            }
        }
    }
    if let Some(busy) = &a.busy {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label(busy.as_str());
        });
    }
    if let Some(e) = &a.error {
        ui.label(egui::RichText::new(e.as_str()).color(ui.visuals().error_fg_color));
    }
    if matches!(a.page, Page::Browse(_)) {
        ui.horizontal(|ui| {
            if showing {
                if big(ui, "Close").clicked() {
                    act = Some(Action::Cancel);
                }
            } else {
                if big(ui, "Back").clicked() {
                    act = Some(Action::Back);
                }
                if big(ui, "Cancel").clicked() {
                    act = Some(Action::Cancel);
                }
            }
        });
    }
    act
}