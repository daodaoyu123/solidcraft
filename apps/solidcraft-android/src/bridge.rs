//! The Java side of the app (MainActivity.java), reached through JNI: the Storage Access
//! Framework pickers, the clipboard and the soft keyboard.
//!
//! SolidCraft reads and writes ordinary files, so a picked document becomes a local file in the
//! app's storage linked to its `content://` URI. Open copies the document in; Save, Save As and
//! the exports write the local file, and [`Links::sync`] copies it back to the document once it
//! changed. The links are kept in `documents/links.json` so a design reopened from Recent still
//! saves to its document.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::task::{Context, Poll};
use std::time::SystemTime;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM};
use solidcraft_ui_egui::FilePickerFuture;

/// Picker answers by request id: `Some(path)` picked, `None` cancelled.
static ANSWERS: Mutex<Vec<(i32, Option<String>)>> = Mutex::new(Vec::new());
/// SMB connection form (Java `AlertDialog`): JSON saved fields, or `None` cancelled.
static SMB_CONN: Mutex<Vec<(i32, Option<String>)>> = Mutex::new(Vec::new());
/// Local files linked to their documents, added by the pickers (local path, URI, and the local
/// file's stamp when linked: a copy of an opened document is in sync; a save target, not yet
/// written, is not, so whatever the app writes there first is copied out).
static NEW_LINKS: Mutex<Vec<(String, String, Option<(SystemTime, u64)>)>> = Mutex::new(Vec::new());
/// Documents opened from other apps, waiting for the app to open them.
static INCOMING: Mutex<Vec<String>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicI32 = AtomicI32::new(1);
/// How much of the window's bottom the soft keyboard covers (px).
/// Each linked local file's document URI, for Show in folder (which runs outside the shell).
static LINK_URIS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

static IME_INSET: AtomicI32 = AtomicI32::new(0);
/// The running MainActivity, registered by its onCreate.
static ACTIVITY: Mutex<Option<GlobalRef>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativeAttach<'l>(env: JNIEnv<'l>, this: JObject<'l>) {
    match env.new_global_ref(&this) {
        Ok(g) => *lock(&ACTIVITY) = Some(g),
        Err(e) => log::error!("JNI attach: {e}"),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativeImeInset<'l>(_env: JNIEnv<'l>, _class: JClass<'l>, px: jint) {
    IME_INSET.store(px.max(0), Ordering::Relaxed);
}

/// The soft keyboard's text since the last frame, as egui IME events.
static IME_TEXT: Mutex<Vec<egui::ImeEvent>> = Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativeImeText<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    kind: jint,
    text: JString<'l>,
    before: jint,
    after: jint,
) {
    let text = opt_string(&mut env, &text).unwrap_or_default();
    let e = match kind {
        0 => egui::ImeEvent::Commit(text),
        1 => egui::ImeEvent::Preedit { text, active_range_chars: None },
        _ => egui::ImeEvent::DeleteSurrounding { before_chars: before.max(0) as usize, after_chars: after.max(0) as usize },
    };
    lock(&IME_TEXT).push(e);
    if let Some(ctx) = EGUI.get() {
        ctx.request_repaint();
    }
}

/// The keyboard's text, composing and deletions since the last call.
pub fn take_ime_text() -> Vec<egui::ImeEvent> {
    std::mem::take(&mut *lock(&IME_TEXT))
}

/// The app's egui context, to wake it when the keyboard types (Java's thread).
pub static EGUI: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// How much of the window's bottom the soft keyboard covers now (px; 0 when hidden).
pub fn ime_inset_px() -> i32 {
    IME_INSET.load(Ordering::Relaxed)
}

/// Open a link in the browser (or whatever app handles it).
pub fn open_url(url: &str) {
    with_activity(|env, act| {
        let u = env.new_string(url)?;
        env.call_method(act, "openUrl", "(Ljava/lang/String;)V", &[JValue::Object(&u)]).map(|_| ())
    });
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn opt_string(env: &mut JNIEnv, s: &JString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(Into::into)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativePicked<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    id: jint,
    local: JString<'l>,
    uri: JString<'l>,
) {
    let local = opt_string(&mut env, &local);
    let uri = opt_string(&mut env, &uri);
    if let (Some(l), Some(u)) = (&local, &uri) {
        lock(&NEW_LINKS).push((l.clone(), u.clone(), stamp(Path::new(l))));
    }
    lock(&ANSWERS).push((id, local));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativeSmbConn<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    token: jint,
    json: JString<'l>,
) {
    let json = opt_string(&mut env, &json);
    lock(&SMB_CONN).push((token, json));
    if let Some(ctx) = EGUI.get() {
        ctx.request_repaint();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_archipelagos_solidcraft_MainActivity_nativeIncoming<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    local: JString<'l>,
    uri: JString<'l>,
) {
    let local = opt_string(&mut env, &local);
    let uri = opt_string(&mut env, &uri);
    if let Some(l) = local {
        if let Some(u) = uri {
            let s = stamp(Path::new(&l));
            lock(&NEW_LINKS).push((l.clone(), u, s));
        }
        lock(&INCOMING).push(l);
    }
}

/// Documents opened from other apps since the last call.
pub fn take_incoming() -> Vec<String> {
    std::mem::take(&mut *lock(&INCOMING))
}

/// Run `f` with the JNI environment and the activity.
fn with_activity<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let ctx = ndk_context::android_context();
    // SAFETY: android-activity fills ndk-context with the process's JavaVM and the activity's
    // global reference before android_main runs; both live as long as the process.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    let Some(activity) = lock(&ACTIVITY).clone() else {
        log::error!("JNI: MainActivity not attached");
        return None;
    };
    let r = f(&mut env, activity.as_obj());
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    match r {
        Ok(r) => Some(r),
        Err(e) => {
            log::error!("JNI: {e}");
            None
        }
    }
}

/// A picker request's answer, polled by the app each frame.
struct Answer(i32);

impl Future for Answer {
    type Output = Option<String>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let mut answers = lock(&ANSWERS);
        match answers.iter().position(|(id, _)| *id == self.0) {
            Some(i) => Poll::Ready(answers.remove(i).1),
            None => Poll::Pending,
        }
    }
}

/// Answer picker request `id` (the location dialog does for network locations).
pub fn answer(id: i32, value: Option<String>) {
    lock(&ANSWERS).push((id, value));
}

/// Open the Android SMB connection dialog (`token` matches `Chooser::Active::token`).
pub fn show_smb_connection_form(token: i32, json: &str) {
    let json = json.to_owned();
    with_activity(|env, act| {
        let j = env.new_string(&json)?;
        env.call_method(act, "smbConnectionForm", "(ILjava/lang/String;)V", &[JValue::Int(token), JValue::Object(&j)]).map(|_| ())
    });
}

pub fn take_smb_conn_answers() -> Vec<(i32, Option<String>)> {
    std::mem::take(&mut *lock(&SMB_CONN))
}

/// Link a local file to its document (`in_sync`: it holds the document now, as an opened copy).
pub fn link(local: &str, uri: &str, in_sync: bool) {
    let s = if in_sync { stamp(Path::new(local)) } else { None };
    lock(&NEW_LINKS).push((local.to_owned(), uri.to_owned(), s));
}

/// Ask Java's picker, answering request `id` (or cancelled when the activity can't).
fn java_request(id: i32, call: impl FnOnce(&mut JNIEnv, &JObject, i32) -> jni::errors::Result<()>) {
    if with_activity(|env, act| call(env, act, id)).is_none() {
        lock(&ANSWERS).push((id, None));
    }
}

/// The system's Open picker (Android Files, cloud drives) for request `id`.
pub fn pick_open_files(id: i32) {
    java_request(id, |env, act, id| env.call_method(act, "pickOpen", "(I)V", &[JValue::Int(id)]).map(|_| ()));
}

/// The system's Save picker (or folder picker for a URDF) for request `id`.
pub fn pick_save_files(id: i32, name: &str) {
    let name = name.to_owned();
    java_request(id, move |env, act, id| {
        let jname = env.new_string(&name)?;
        env.call_method(act, "pickSave", "(ILjava/lang/String;)V", &[JValue::Int(id), JValue::Object(&jname)]).map(|_| ())
    });
}

/// Open: the location dialog (Android Files or a network location) answers.
pub fn pick_open(_extensions: &[&str]) -> FilePickerFuture {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    crate::smb_ui::request(crate::smb_ui::Request { id, kind: crate::smb_ui::Kind::Open });
    Box::pin(Answer(id))
}

pub fn pick_save(name: &str, extensions: &[&str]) -> FilePickerFuture {
    // The suggested name keeps the extension the command writes (the first offered).
    let name = match (Path::new(name).extension(), extensions.first()) {
        (None, Some(ext)) => format!("{name}.{ext}"),
        _ => name.to_string(),
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    crate::smb_ui::request(crate::smb_ui::Request { id, kind: crate::smb_ui::Kind::Save(name) });
    Box::pin(Answer(id))
}

/// Network uploads finished on their threads: the local file, the stamp uploaded, the message.
static UPLOADS: Mutex<Vec<(String, (SystemTime, u64), Result<String, String>)>> = Mutex::new(Vec::new());
/// Local files being uploaded now.
static UPLOADING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Upload a changed local file (or a URDF's folder) to its network document, on a thread.
fn upload(local: &str, uri: &str, at: (SystemTime, u64)) -> Result<(), String> {
    let name = Path::new(local).file_name().map_or_else(|| local.to_owned(), |n| n.to_string_lossy().into_owned());
    let Some((cid, remote, tree)) = crate::smb_core::parse_uri(uri) else { return Err(format!("Couldn't save {name}: not a network address")) };
    let Some(conn) = crate::smb_ui::conn(&cid) else { return Err(format!("Couldn't save {name}: its network location was removed")) };
    lock(&UPLOADING).get_or_insert_with(HashSet::new).insert(local.to_owned());
    let local = local.to_owned();
    let shown = name.clone();
    let r = std::thread::Builder::new().name("smb-upload".into()).spawn(move || {
        let name = shown;
        let pw = crate::smb_ui::stored_password(conn.id.clone());
        let r = if tree {
            match Path::new(&local).parent() {
                Some(dir) => crate::smb_core::upload_tree(&conn, &pw, dir, &remote),
                None => Err(crate::smb_core::SmbError::Other("no folder".into())),
            }
        } else {
            crate::smb_core::upload(&conn, &pw, Path::new(&local), &remote)
        };
        let msg = match r {
            Ok(_) => {
                log::info!("uploaded {local} to {}", conn.label());
                Ok(format!("Saved {name} to {}", conn.label()))
            }
            Err(e) => Err(format!("Couldn't save {name} to {}: {e}", conn.label())),
        };
        lock(&UPLOADS).push((local, at, msg));
        if let Some(ctx) = EGUI.get() {
            ctx.request_repaint();
        }
    });
    r.map(|_| ()).map_err(|e| format!("Couldn't save {name}: {e}"))
}

fn write_back(local: &str, uri: &str) -> bool {
    with_activity(|env, act| {
        let l = env.new_string(local)?;
        let u = env.new_string(uri)?;
        env.call_method(act, "writeBack", "(Ljava/lang/String;Ljava/lang/String;)Z", &[JValue::Object(&l), JValue::Object(&u)])?.z()
    })
    .unwrap_or(false)
}

pub fn set_clipboard(text: &str) {
    with_activity(|env, act| {
        let t = env.new_string(text)?;
        env.call_method(act, "setClipboard", "(Ljava/lang/String;)V", &[JValue::Object(&t)]).map(|_| ())
    });
}

pub fn clipboard() -> Option<String> {
    with_activity(|env, act| {
        let o = env.call_method(act, "getClipboard", "()Ljava/lang/String;", &[])?.l()?;
        if o.is_null() {
            return Ok(None);
        }
        let s = JString::from(o);
        Ok(Some(env.get_string(&s)?.into()))
    })
    .flatten()
}

/// Show or hide the soft keyboard; `password`: for a password field (hidden, nothing learnt).
pub fn show_keyboard(show: bool, password: bool) {
    with_activity(|env, act| env.call_method(act, "showKeyboard", "(ZZ)V", &[JValue::Bool(u8::from(show)), JValue::Bool(u8::from(password))]).map(|_| ()));
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

struct Link {
    uri: String,
    /// The local file as last copied in or written back (`None`: not written yet).
    synced: Option<(SystemTime, u64)>,
}

/// Local files and their documents.
pub struct Links {
    store: Option<PathBuf>,
    links: HashMap<String, Link>,
    checked: f64,
}

impl Links {
    pub fn load(store: Option<PathBuf>) -> Self {
        let mut links = HashMap::new();
        if let Some(text) = store.as_ref().and_then(|p| std::fs::read_to_string(p).ok())
            && let Ok(saved) = serde_json::from_str::<HashMap<String, String>>(&text)
        {
            for (local, uri) in saved {
                let synced = stamp(Path::new(&local));
                links.insert(local, Link { uri, synced });
            }
        }
        let me = Self { store, links, checked: f64::NEG_INFINITY };
        me.publish();
        me
    }

    fn publish(&self) {
        // Kept after an export's copy is removed: its Show in folder still finds the document.
        lock(&LINK_URIS).get_or_insert_with(HashMap::new).extend(self.links.iter().map(|(l, k)| (l.clone(), k.uri.clone())));
    }

    fn save(&self) {
        self.publish();
        let Some(store) = &self.store else { return };
        let map: HashMap<&str, &str> = self.links.iter().map(|(l, k)| (l.as_str(), k.uri.as_str())).collect();
        if let Some(dir) = store.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(store, text);
        }
    }

    /// Take new links, and copy local files that changed back to their documents. Not while a
    /// save is being written (`busy`); a file the app wrote is replaced whole (written beside it
    /// and renamed), so a changed one is complete.
    pub fn sync(&mut self, now: f64, busy: bool) -> Vec<Result<String, String>> {
        let new: Vec<_> = std::mem::take(&mut *lock(&NEW_LINKS));
        let added = !new.is_empty();
        for (local, uri, synced) in new {
            self.links.insert(local, Link { uri, synced });
        }
        if added {
            self.save();
        }
        let mut done = Vec::new();
        let mut finished = Vec::new();
        // Network uploads that finished: an export's copy goes only once it is on the server.
        for (local, at, r) in std::mem::take(&mut *lock(&UPLOADS)) {
            if let Some(u) = lock(&UPLOADING).as_mut() {
                u.remove(&local);
            }
            if let Some(link) = self.links.get_mut(&local) {
                link.synced = Some(at);
                if r.is_ok() && is_one_shot(&local, &link.uri) && stamp(Path::new(&local)) == Some(at) {
                    finished.push(local.clone());
                }
            }
            done.push(r);
        }
        if !(busy || now - self.checked < 0.4) {
            self.checked = now;
            let uploading = lock(&UPLOADING).clone().unwrap_or_default();
            for (local, link) in &mut self.links {
                let Some(now_stamp) = stamp(Path::new(local)) else { continue };
                if link.synced == Some(now_stamp) || uploading.contains(local) {
                    continue;
                }
                let name = Path::new(local).file_name().map_or_else(|| local.clone(), |n| n.to_string_lossy().into_owned());
                if crate::smb_core::parse_uri(&link.uri).is_some() {
                    // Off the window's thread; the answer comes back through UPLOADS.
                    if let Err(e) = upload(local, &link.uri, now_stamp) {
                        link.synced = Some(now_stamp);
                        done.push(Err(e));
                    }
                    continue;
                }
                if write_back(local, &link.uri) {
                    link.synced = Some(now_stamp);
                    log::info!("wrote {local} back to {}", link.uri);
                    done.push(Ok(format!("Saved {name} to its document")));
                    if is_one_shot(local, &link.uri) {
                        finished.push(local.clone());
                    }
                } else {
                    // Tried once per change: a document that can't be written isn't retried each frame.
                    link.synced = Some(now_stamp);
                    done.push(Err(format!("Couldn't write {name} back to its document")));
                }
            }
        }
        if !finished.is_empty() {
            for local in finished {
                self.links.remove(&local);
                remove_export(Path::new(&local));
            }
            self.save();
        }
        done
    }
}

/// An API key from the Keystore-backed store: `Ok(None)` when there is none.
pub fn secret_get(id: &str) -> Result<Option<String>, String> {
    secret_get_in("secretGet", id)
}

/// Store (`Some`) or delete (`None`) an API key.
pub fn secret_set(id: &str, secret: Option<&str>) -> Result<(), String> {
    secret_set_in("secretSet", "secretDelete", id, secret)
}

/// A network location's password from its own Keystore-backed store (not the agents' keys).
pub fn smb_secret_get(id: &str) -> Result<Option<String>, String> {
    secret_get_in("smbSecretGet", id)
}

/// Store (`Some`) or delete (`None`) a network location's password.
pub fn smb_secret_set(id: &str, secret: Option<&str>) -> Result<(), String> {
    secret_set_in("smbSecretSet", "smbSecretDelete", id, secret)
}

fn secret_get_in(method: &str, id: &str) -> Result<Option<String>, String> {
    with_activity(|env, act| {
        let i = env.new_string(id)?;
        let o = env.call_method(act, method, "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&i)])?.l()?;
        if o.is_null() {
            return Ok(None);
        }
        let s = JString::from(o);
        Ok(Some(env.get_string(&s)?.into()))
    })
    .ok_or_else(|| "the Android Keystore could not read the key".to_owned())
}

fn secret_set_in(set: &str, delete: &str, id: &str, secret: Option<&str>) -> Result<(), String> {
    with_activity(|env, act| {
        let i = env.new_string(id)?;
        match secret {
            Some(s) => {
                let s = env.new_string(s)?;
                env.call_method(act, set, "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&i), JValue::Object(&s)])?;
            }
            None => {
                env.call_method(act, delete, "(Ljava/lang/String;)V", &[JValue::Object(&i)])?;
            }
        }
        Ok(())
    })
    .ok_or_else(|| "the Android Keystore could not store the key".to_owned())
}

/// Show in folder: the folder of the document `path` is linked to, in Files.
pub fn show_in_folder(path: &str) {
    let uri = lock(&LINK_URIS).as_ref().and_then(|m| m.get(path).cloned());
    // A network document: its folder in the app's network browser.
    if let Some((cid, remote, tree)) = uri.as_deref().and_then(crate::smb_core::parse_uri) {
        let dir = if tree { remote } else { crate::smb_core::parent(&remote) };
        crate::smb_ui::request(crate::smb_ui::Request { id: -1, kind: crate::smb_ui::Kind::Show(cid, dir) });
        return;
    }
    with_activity(|env, act| {
        let u: JObject = match &uri {
            Some(u) => env.new_string(u)?.into(),
            None => JObject::null(),
        };
        env.call_method(act, "showInFolder", "(Ljava/lang/String;)V", &[JValue::Object(&u)]).map(|_| ())
    });
}
/// An export written to its document (or folder) for good: nothing reopens the local copy, so it
/// is deleted once written back. Designs and libraries stay, as Save writes them again.
fn is_one_shot(local: &str, uri: &str) -> bool {
    const EXPORTS: &[&str] = &["stl", "3mf", "obj", "step", "stp", "iges", "igs", "glb", "gltf", "dxf", "svg", "pdf", "png", "gif", "csv", "urdf"];
    let ext = Path::new(local).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    uri.starts_with("tree:") || uri.starts_with("smbtree:") || EXPORTS.contains(&ext.as_str())
}

/// Delete an export's local copy: its folder in the app's storage (made for this one file, or
/// for a URDF and its meshes).
fn remove_export(local: &Path) {
    let Some(dir) = local.parent() else { return };
    // Only a folder of ours: <files>/documents/<fresh>/.
    if dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "documents") {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => log::info!("removed export copy {}", dir.display()),
            Err(e) => log::warn!("could not remove {}: {e}", dir.display()),
        }
    } else {
        let _ = std::fs::remove_file(local);
    }
}

/// At start: the empty folders cancelled saves leave in the app's storage are removed (folders
/// with files stay: they may hold a design whose link was lost).
pub fn sweep_documents(root: &Path, links: &Links) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let linked: std::collections::HashSet<&Path> = links.links.keys().filter_map(|l| Path::new(l).parent()).collect();
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.is_dir() || linked.contains(dir.as_path()) {
            continue;
        }
        let empty = std::fs::read_dir(&dir).map(|mut d| d.next().is_none()).unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}
/// One HTTP request through MainActivity.http: the status and body, or why it failed.
pub fn http(method: &str, url: &str, headers: &[(String, String)], body: Option<&[u8]>, timeout_s: u64) -> Result<(u16, Vec<u8>), String> {
    let out = with_activity(|env, act| {
        let m = env.new_string(method)?;
        let u = env.new_string(url)?;
        let h = env.new_object_array((headers.len() * 2) as i32, "java/lang/String", JObject::null())?;
        for (i, (k, v)) in headers.iter().enumerate() {
            let k = env.new_string(k)?;
            let v = env.new_string(v)?;
            env.set_object_array_element(&h, (i * 2) as i32, &k)?;
            env.set_object_array_element(&h, (i * 2 + 1) as i32, &v)?;
        }
        let b: JObject = match body {
            Some(b) => env.byte_array_from_slice(b)?.into(),
            None => JObject::null(),
        };
        let r = env
            .call_method(
                act,
                "http",
                "(Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;[BI)[B",
                &[JValue::Object(&m), JValue::Object(&u), JValue::Object(&h), JValue::Object(&b), JValue::Int(timeout_s as i32)],
            )?
            .l()?;
        env.convert_byte_array(jni::objects::JByteArray::from(r))
    })
    .ok_or_else(|| "the request could not be sent".to_owned())?;
    // The status (4 bytes, big-endian) then the body; status 0: the body is the error.
    if out.len() < 4 {
        return Err("no response".to_owned());
    }
    let status = u32::from_be_bytes([out[0], out[1], out[2], out[3]]);
    let rest = out[4..].to_vec();
    if status == 0 {
        return Err(String::from_utf8_lossy(&rest).into_owned());
    }
    Ok((status.min(u32::from(u16::MAX)) as u16, rest))
}