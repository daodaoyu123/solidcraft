//! Network (SMB) locations: blocking SMB 2/3 calls through the pure-Rust `smb` crate (its
//! multi-threaded build), always run on worker threads, never on the window's thread.
//!
//! A connection is a host and a share (and a user, or none for guest). Paths inside the share
//! are relative, with `/` between folders; "" is the share's top folder. Signing is required
//! except for guest sessions; encryption is used whenever the server offers it. NTLM signs in.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use smb::{
    Client, ClientConfig, CreateDisposition, CreateOptions, FileAccessMask, FileAttributes, FileCreateArgs,
    FileDirectoryInformation, GetLen, ReadAt, UncPath, WriteAt,
};

/// A saved network location. The password is not here: it is kept by the platform's secret store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conn {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub host: String,
    pub share: String,
    /// Empty: a guest session.
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub domain: String,
}

impl Conn {
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() { format!("{} \\ {}", self.host.trim(), self.share.trim()) } else { self.name.trim().to_owned() }
    }
    pub fn guest(&self) -> bool {
        self.user.trim().is_empty()
    }
    fn username(&self) -> String {
        if self.guest() {
            "Guest".to_owned()
        } else if self.domain.trim().is_empty() {
            self.user.trim().to_owned()
        } else {
            format!("{}\\{}", self.domain.trim(), self.user.trim())
        }
    }
}

/// One entry of a network folder.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// Why a network call failed, in words for the person using the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmbError {
    /// The host's name could not be found.
    NoHost(String),
    /// Nothing answered on the host's SMB port (445).
    Unreachable(String),
    /// The server refused the user name or password.
    Auth,
    /// Signed in, but this user may not do that here.
    Denied,
    NoShare(String),
    NotFound(String),
    /// No password is stored for a connection with a user.
    NoPassword,
    Other(String),
}

impl std::fmt::Display for SmbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHost(h) => write!(f, "Couldn't find the host \"{h}\". Check the name, or use its IP address (for example 192.168.1.20)."),
            Self::Unreachable(h) => write!(f, "Can't reach {h} on the network (SMB port 445). Check the address, Wi-Fi, and that the server is on."),
            Self::Auth => write!(f, "Sign-in failed: the server refused the user name or password."),
            Self::Denied => write!(f, "Permission denied: this user may not do that in this folder."),
            Self::NoShare(s) => write!(f, "The server has no share named \"{s}\"."),
            Self::NotFound(p) => write!(f, "Not found: {p}"),
            Self::NoPassword => write!(f, "No password is saved for this connection. Edit it and enter the password."),
            Self::Other(e) => write!(f, "Network error: {e}"),
        }
    }
}

const TIMEOUT: Duration = Duration::from_secs(8);
/// Read and write size: 64 KiB works with every SMB 2/3 dialect without multi-credit requests.
const CHUNK: usize = 1 << 16;

const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
const STATUS_OBJECT_NAME_COLLISION: u32 = 0xC000_0035;
const STATUS_OBJECT_PATH_NOT_FOUND: u32 = 0xC000_003A;
const STATUS_LOGON_FAILURE: u32 = 0xC000_006D;
const STATUS_ACCOUNT_RESTRICTION: u32 = 0xC000_006E;
const STATUS_WRONG_PASSWORD: u32 = 0xC000_006A;
const STATUS_NO_SUCH_USER: u32 = 0xC000_0064;
const STATUS_ACCOUNT_DISABLED: u32 = 0xC000_0072;
const STATUS_BAD_NETWORK_NAME: u32 = 0xC000_00CC;

fn config(guest: bool) -> ClientConfig {
    let mut c = ClientConfig::default();
    c.dfs = false;
    c.connection.timeout = Some(TIMEOUT);
    // Unsigned traffic only for a guest session (servers do not sign those).
    c.connection.allow_unsigned_guest_access = guest;
    c
}

fn status(e: &smb::Error) -> Option<u32> {
    match e {
        smb::Error::ReceivedErrorMessage(s, _) | smb::Error::UnexpectedMessageStatus(s) => Some(*s),
        _ => None,
    }
}

/// An error in the app's words. `host`/`what` name what was asked for.
fn classify(e: &smb::Error, host: &str, share: &str, what: &str) -> SmbError {
    match status(e) {
        Some(STATUS_LOGON_FAILURE | STATUS_WRONG_PASSWORD | STATUS_NO_SUCH_USER | STATUS_ACCOUNT_RESTRICTION | STATUS_ACCOUNT_DISABLED) => return SmbError::Auth,
        Some(STATUS_ACCESS_DENIED) => return SmbError::Denied,
        Some(STATUS_BAD_NETWORK_NAME) => return SmbError::NoShare(share.to_owned()),
        Some(STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND) => return SmbError::NotFound(what.to_owned()),
        _ => {}
    }
    let text = e.to_string();
    let low = text.to_lowercase();
    if matches!(e, smb::Error::SspiError(_)) || low.contains("logon failure") {
        return SmbError::Auth;
    }
    if low.contains("invalid address") || low.contains("lookup") || low.contains("resolve") || low.contains("no such host") || low.contains("name or service") {
        return SmbError::NoHost(host.to_owned());
    }
    if low.contains("timed out")
        || low.contains("refused")
        || low.contains("unreachable")
        || low.contains("not connected")
        || low.contains("connection reset")
        || low.contains("os error 110")
        || low.contains("os error 111")
        || low.contains("os error 113")
        || low.contains("os error 101")
    {
        return SmbError::Unreachable(host.to_owned());
    }
    SmbError::Other(text)
}

/// Clients by connection id, kept signed in between calls.
static CLIENTS: Mutex<Option<HashMap<String, Arc<Client>>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sign out of a connection (after it was edited or removed, or failed).
pub fn forget(id: &str) {
    let gone = lock(&CLIENTS).as_mut().and_then(|m| m.remove(id));
    if let Some(c) = gone {
        let _ = c.close();
    }
}

fn root(c: &Conn) -> Result<UncPath, SmbError> {
    let host = c.host.trim();
    let share = c.share.trim().trim_matches(['\\', '/']);
    if host.is_empty() {
        return Err(SmbError::Other("enter the host (a name or an IP address)".to_owned()));
    }
    if share.is_empty() {
        return Err(SmbError::Other("enter the share's name".to_owned()));
    }
    let host = host.trim_start_matches("\\\\").trim_start_matches("smb://");
    UncPath::new(host).and_then(|u| u.with_share(share)).map_err(|e| SmbError::Other(e.to_string()))
}

fn at(c: &Conn, path: &str) -> Result<UncPath, SmbError> {
    let p = path.trim_matches('/');
    let r = root(c)?;
    Ok(if p.is_empty() { r } else { r.with_path(p) })
}

/// A signed-in client for the connection. `password` is asked only when signing in.
fn client(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>) -> Result<Arc<Client>, SmbError> {
    if let Some(cl) = lock(&CLIENTS).as_ref().and_then(|m| m.get(&c.id).cloned()) {
        return Ok(cl);
    }
    let unc = root(c)?;
    let pw = if c.guest() {
        String::new()
    } else {
        match password().map_err(SmbError::Other)? {
            Some(p) => p,
            None => return Err(SmbError::NoPassword),
        }
    };
    let cl = Client::new(config(c.guest()));
    if let Err(e) = cl.share_connect(&unc, &c.username(), pw) {
        let _ = cl.close();
        return Err(classify(&e, c.host.trim(), c.share.trim(), ""));
    }
    let cl = Arc::new(cl);
    lock(&CLIENTS).get_or_insert_with(HashMap::new).insert(c.id.clone(), cl.clone());
    Ok(cl)
}

/// Run `f` signed in; a dropped session (server restart, Wi-Fi change) signs in again once.
fn signed_in<R>(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, what: &str, f: impl Fn(&Client) -> smb::Result<R>) -> Result<R, SmbError> {
    let cl = client(c, password)?;
    match f(&cl) {
        Ok(r) => Ok(r),
        Err(e) => {
            let first = classify(&e, c.host.trim(), c.share.trim(), what);
            if matches!(first, SmbError::Denied | SmbError::NotFound(_) | SmbError::Auth | SmbError::NoShare(_)) {
                return Err(first);
            }
            log::info!("SMB: {e}; signing in again");
            forget(&c.id);
            let cl = client(c, password)?;
            f(&cl).map_err(|e| classify(&e, c.host.trim(), c.share.trim(), what))
        }
    }
}

/// Reach the host and agree a dialect, without signing in: the dialect's name.
pub fn probe(host: &str) -> Result<String, SmbError> {
    let host = host.trim().trim_start_matches("\\\\");
    if host.is_empty() {
        return Err(SmbError::Other("enter the host (a name or an IP address)".to_owned()));
    }
    let cl = Client::new(config(false));
    let r = cl.connect(host).map(|conn| conn.conn_info().map(|i| format!("{:?}", i.negotiation.dialect_rev)).unwrap_or_else(|| "SMB".to_owned()));
    let _ = cl.close();
    r.map_err(|e| classify(&e, host, "", ""))
}

/// Sign in and look at the share's top folder: checks a connection as entered.
pub fn check(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>) -> Result<usize, SmbError> {
    forget(&c.id);
    list(c, password, "").map(|e| e.len())
}

/// A folder's entries: folders first, then files, each by name.
pub fn list(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, dir: &str) -> Result<Vec<Entry>, SmbError> {
    let unc = at(c, dir)?;
    let mut out = signed_in(c, password, dir, |cl| {
        let args = FileCreateArgs {
            disposition: CreateDisposition::Open,
            attributes: FileAttributes::new(),
            options: CreateOptions::new().with_directory_file(true),
            desired_access: FileAccessMask::new().with_generic_read(true),
        };
        let d = cl.create_file(&unc, &args)?.unwrap_dir();
        let mut v = Vec::new();
        let r = (|| -> smb::Result<()> {
            for item in d.query::<FileDirectoryInformation>("*")? {
                let item = item?;
                let name = item.file_name.to_string();
                if name == "." || name == ".." {
                    continue;
                }
                v.push(Entry { name, dir: item.file_attributes.directory(), size: item.end_of_file });
            }
            Ok(())
        })();
        let _ = d.close();
        r.map(|()| v)
    })?;
    out.retain(|e| !e.name.starts_with('.') || e.dir);
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

/// Copy a network file to a local one (replaced whole: written beside it, then renamed).
pub fn download(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, remote: &str, local: &Path) -> Result<u64, SmbError> {
    let unc = at(c, remote)?;
    let part = local.with_extension("smb-part");
    let n = signed_in(c, password, remote, |cl| {
        let args = FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true));
        let f = cl.create_file(&unc, &args)?.unwrap_file();
        let r = (|| -> smb::Result<u64> {
            let len = f.get_len()?;
            let mut out = std::fs::File::create(&part)?;
            let mut buf = vec![0u8; CHUNK];
            let mut off = 0u64;
            while off < len {
                let want = CHUNK.min((len - off) as usize);
                let n = f.read_at(&mut buf[..want], off)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                off += n as u64;
            }
            out.sync_all()?;
            Ok(off)
        })();
        let _ = f.close();
        r
    });
    match n {
        Ok(n) => {
            std::fs::rename(&part, local).map_err(|e| SmbError::Other(format!("could not keep the copy: {e}")))?;
            Ok(n)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Write a local file to the network, replacing what is there.
pub fn upload(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, local: &Path, remote: &str) -> Result<u64, SmbError> {
    let unc = at(c, remote)?;
    let mut data = Vec::new();
    std::fs::File::open(local).and_then(|mut f| f.read_to_end(&mut data)).map_err(|e| SmbError::Other(format!("could not read {}: {e}", local.display())))?;
    signed_in(c, password, remote, |cl| {
        let args = FileCreateArgs {
            disposition: CreateDisposition::OverwriteIf,
            attributes: FileAttributes::new(),
            options: CreateOptions::new().with_non_directory_file(true),
            desired_access: FileAccessMask::new().with_generic_read(true).with_generic_write(true),
        };
        let f = cl.create_file(&unc, &args)?.unwrap_file();
        let r = (|| -> smb::Result<u64> {
            let mut off = 0usize;
            while off < data.len() {
                let end = (off + CHUNK).min(data.len());
                let n = f.write_at(&data[off..end], off as u64)?;
                if n == 0 {
                    return Err(smb::Error::Other("the server wrote nothing"));
                }
                off += n;
            }
            Ok(off as u64)
        })();
        let _ = f.close();
        r
    })
}

/// Make a folder (fine when it is there already).
pub fn mkdir(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, dir: &str) -> Result<(), SmbError> {
    let unc = at(c, dir)?;
    signed_in(c, password, dir, |cl| {
        let args = FileCreateArgs {
            disposition: CreateDisposition::OpenIf,
            attributes: FileAttributes::new().with_directory(true),
            options: CreateOptions::new().with_directory_file(true),
            desired_access: FileAccessMask::new().with_generic_read(true).with_generic_write(true),
        };
        match cl.create_file(&unc, &args) {
            Ok(r) => {
                if r.is_dir() { let _ = r.unwrap_dir().close(); }
                Ok(())
            }
            Err(e) if status(&e) == Some(STATUS_OBJECT_NAME_COLLISION) => Ok(()),
            Err(e) => Err(e),
        }
    })
}

/// Copy a local folder (a URDF and its meshes) into a network folder, replacing same-named files.
pub fn upload_tree(c: &Conn, password: &dyn Fn() -> Result<Option<String>, String>, local: &Path, remote: &str) -> Result<u64, SmbError> {
    let entries = std::fs::read_dir(local).map_err(|e| SmbError::Other(format!("could not read {}: {e}", local.display())))?;
    let mut total = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let to = join(remote, &name);
        let p = e.path();
        if p.is_dir() {
            mkdir(c, password, &to)?;
            total += upload_tree(c, password, &p, &to)?;
        } else {
            total += upload(c, password, &p, &to)?;
        }
    }
    Ok(total)
}

/// A path inside a share: `dir` and a name, with `/` between.
pub fn join(dir: &str, name: &str) -> String {
    let d = dir.trim_matches('/');
    if d.is_empty() { name.to_owned() } else { format!("{d}/{name}") }
}

/// The folder a path is in ("" at the top).
pub fn parent(path: &str) -> String {
    let p = path.trim_matches('/');
    p.rsplit_once('/').map(|(d, _)| d.to_owned()).unwrap_or_default()
}

/// A linked document's address: `smb://<connection id>/<path>` (`smbtree://` for a folder).
pub fn uri(id: &str, path: &str, tree: bool) -> String {
    format!("{}://{id}/{}", if tree { "smbtree" } else { "smb" }, path.trim_matches('/'))
}

/// The connection id, path, and whether it is a folder, of an address made by [`uri`].
pub fn parse_uri(u: &str) -> Option<(String, String, bool)> {
    let (rest, tree) = if let Some(r) = u.strip_prefix("smbtree://") {
        (r, true)
    } else {
        (u.strip_prefix("smb://")?, false)
    };
    let (id, path) = rest.split_once('/').unwrap_or((rest, ""));
    Some((id.to_owned(), path.to_owned(), tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let u = uri("k3x-0", "Designs/Pi 5/tin.solidcraft", false);
        assert_eq!(u, "smb://k3x-0/Designs/Pi 5/tin.solidcraft");
        assert_eq!(parse_uri(&u), Some(("k3x-0".into(), "Designs/Pi 5/tin.solidcraft".into(), false)));
        assert_eq!(parse_uri("smbtree://a/x"), Some(("a".into(), "x".into(), true)));
        assert_eq!(parse_uri("content://x"), None);
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a/b/", "c"), "a/b/c");
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
    }
}