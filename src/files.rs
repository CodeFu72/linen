// Files, dialogs (zenity), settings and the recovery session. Saves are atomic, keep
// the file's permissions, and notice when a file changed on disk since Linen opened it.
// Adapted from Velvet.

use crate::lock;
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// A temp file next to `target` that no one else can read or race (review fix #3): a
/// unique name, created fresh (never follows an old file or link), private from the
/// first byte.
fn private_temp(target: &Path, mode: u32) -> std::io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = target.parent().unwrap_or(Path::new("."));
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{}.{}-{n}.linen-tmp", file_name(target), std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

/// The Obsidian vault: LINEN_VAULT, else the folder chosen once in "Save to Obsidian"
/// (kept in settings as `vault_dir`). No personal path is built in (review fix #8).
pub fn vault_dir() -> Option<PathBuf> {
    std::env::var("LINEN_VAULT").ok().filter(|v| !v.is_empty()).map(PathBuf::from)
        .or_else(|| settings()["vault_dir"].as_str().filter(|v| !v.is_empty()).map(PathBuf::from))
}

pub fn projects_dir() -> PathBuf {
    std::env::var("LINEN_PROJECTS").map(PathBuf::from).unwrap_or_else(|_| home().join("Projects"))
}

fn config_path() -> PathBuf {
    home().join(".config/linen/settings.json")
}

fn session_path() -> PathBuf {
    home().join(".local/share/linen/session.json")
}

pub fn mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::metadata(path).map_err(|e| format!("Can't open {}: {e}", path.display()))?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{} is larger than 10 MB.", path.display()));
    }
    fs::read(path).map_err(|e| e.to_string())
}

/// A locked note comes back without text; the UI asks for the password and calls `unlock`.
pub fn read(path: &Path) -> Result<Value, String> {
    let bytes = read_bytes(path)?;
    if lock::is_locked(path, &bytes) {
        return Ok(json!({ "path": path, "name": file_name(path), "locked": true, "mtime": mtime(path) }));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{} isn't plain text.", path.display()))?;
    Ok(json!({ "path": path, "name": file_name(path), "text": text, "mtime": mtime(path) }))
}

pub fn unlock(path: &Path, password: &str) -> Result<Value, String> {
    let text = lock::decrypt(&read_bytes(path)?, password)?;
    Ok(json!({ "path": path, "name": file_name(path), "text": text, "locked": true, "mtime": mtime(path) }))
}

/// Saves a locked note: encrypted before it touches the disk.
pub fn save_locked(path: &Path, text: &str, password: &str, expected: Option<u64>, force: bool) -> Result<Value, String> {
    save_bytes(path, &lock::encrypt(text, password)?, expected, force)
}

/// 🔒 Lock the open note. A saved note becomes `<name>.gpg` next to it and its plain
/// copy is removed, only after the locked copy is checked to open with the password.
/// An unsaved note asks where to go. An already-locked note just takes the new password.
pub fn lock(path: &str, text: &str, password: &str, dir: &str, name: &str) -> Result<Value, String> {
    // Work on the real file, so locking a link never leaves the real plain copy behind (review fix #7).
    let plain = (!path.is_empty()).then(|| fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path)));
    let target = match &plain {
        Some(p) if p.extension().is_some_and(|e| e == "gpg") => p.clone(),
        Some(p) => {
            let base = p.parent().ok_or("No folder to save into")?;
            let mut t = base.join(format!("{}.gpg", file_name(p)));
            let mut n = 2;
            while t.exists() {
                t = base.join(format!("{} {n}.gpg", file_name(p)));
                n += 1;
            }
            t
        }
        None => match pick_save(dir, &format!("{name}.gpg"))? {
            Some(p) if p.extension().is_some_and(|e| e == "gpg") => p,
            Some(p) => {
                // The dialog only asked about the name typed; ".gpg" is added here, so check again.
                let t = PathBuf::from(format!("{}.gpg", p.display()));
                if t.exists() {
                    return Err(format!("A locked note called {} already exists, so nothing was changed. Pick another name.", file_name(&t)));
                }
                t
            }
            None => return Ok(json!({ "cancelled": true })),
        },
    };
    let locked = lock::encrypt(text, password)?;
    if lock::decrypt(&locked, password)? != text {
        return Err("The locked copy didn't check out, so nothing was changed.".into());
    }
    let mut r = save_bytes(&target, &locked, None, true)?;
    if let Some(p) = plain.filter(|p| *p != target && p.exists()) {
        fs::remove_file(&p).map_err(|e| format!("Locked, but couldn't remove the plain copy {}: {e}", p.display()))?;
        r["removed"] = json!(p);
    }
    Ok(r)
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Atomic replace that keeps the file's permissions. `expected` is the mtime Linen
/// saw when it opened the file; a different one means someone else changed it.
pub fn save(path: &Path, text: &str, expected: Option<u64>, force: bool) -> Result<Value, String> {
    // Safety net: never write plain text over a locked note, by name or by contents.
    let head = fs::File::open(path).ok().and_then(|mut f| {
        let mut b = [0u8; 1];
        std::io::Read::read(&mut f, &mut b).ok().filter(|n| *n == 1).map(|_| b)
    });
    if path.extension().is_some_and(|e| e == "gpg") || head.is_some_and(|b| lock::is_locked(Path::new(""), &b)) {
        return Err("This is a locked note, so it can only be saved locked.".into());
    }
    save_bytes(path, text.as_bytes(), expected, force)
}

fn save_bytes(path: &Path, bytes: &[u8], expected: Option<u64>, force: bool) -> Result<Value, String> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let (Some(seen), false) = (expected, force) {
        let now = mtime(&target);
        if now != 0 && seen != 0 && now != seen {
            return Ok(json!({ "conflict": true }));
        }
    }
    let mode = fs::metadata(&target).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
    target.parent().ok_or("No folder to save into")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Notes over 10 MB can't be saved (Linen couldn't open them again).".into());
    }
    let (tmp, mut f) = private_temp(&target, 0o600).map_err(|e| format!("Couldn't save: {e}"))?;
    let mut write = || -> std::io::Result<()> {
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, &target)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Couldn't save: {e}")
    })?;
    Ok(json!({ "path": target, "name": file_name(&target), "mtime": mtime(&target) }))
}

/// Ok(None) means the person cancelled; Err means the dialog couldn't open at all.
fn zenity(args: &[&str]) -> Result<Option<String>, String> {
    let out = Command::new("zenity")
        .args(args)
        .output()
        .map_err(|_| "Linen uses zenity for its file boxes, and it isn't installed. On Omarchy run: omarchy pkg add zenity".to_string())?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((out.status.success() && !s.is_empty()).then_some(s))
}

fn start_dir(hint: &str) -> String {
    let p = if hint.is_empty() { home() } else { PathBuf::from(hint) };
    format!("{}/", p.display())
}

pub fn pick_files(hint: &str) -> Result<Vec<PathBuf>, String> {
    Ok(zenity(&["--file-selection", "--multiple", "--separator=\n", "--title=Open in Linen", &format!("--filename={}", start_dir(hint))])?
        .map(|s| s.lines().map(PathBuf::from).collect())
        .unwrap_or_default())
}

pub fn pick_folder(hint: &str, title: &str) -> Result<Option<PathBuf>, String> {
    Ok(zenity(&["--file-selection", "--directory", &format!("--title={title}"), &format!("--filename={}", start_dir(hint))])?.map(PathBuf::from))
}

/// True when `dir`, with links resolved, is `root` or inside it (review fix #13).
fn inside(root: &Path, dir: &Path) -> bool {
    match (fs::canonicalize(root), fs::canonicalize(dir)) {
        (Ok(r), Ok(d)) => d.starts_with(r),
        _ => false,
    }
}

pub fn pick_save(dir: &str, name: &str) -> Result<Option<PathBuf>, String> {
    // A slash in a suggested name would turn into folders in the dialog.
    let name = name.replace('/', "-");
    let suggested = format!("{}{name}", start_dir(dir));
    Ok(zenity(&["--file-selection", "--save", "--confirm-overwrite", "--title=Save from Linen", &format!("--filename={suggested}")])?.map(PathBuf::from))
}

/// A file name from a title: no slashes or odd characters, an extension, and never
/// the name of a file that's already there.
pub fn free_name(dir: &Path, title: &str, ext: &str) -> PathBuf {
    let clean: String = title.chars().filter(|c| !"/\\:*?\"<>|#^[]\n\r\t".contains(*c)).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem = match clean.trim().trim_matches('.') {
        "" => "Untitled".to_string(),
        s => s.chars().take(80).collect::<String>().trim().to_string(),
    };
    let ext = if ext == "md" { "md" } else { "txt" };
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} {n}.{ext}"));
        n += 1;
    }
    path
}

/// Save a note into a vault folder you pick; never overwrites an existing note.
pub fn save_to_vault(text: &str, title: &str) -> Result<Value, String> {
    // First time: ask where the vault is, and remember it.
    let vault = match vault_dir() {
        Some(v) => v,
        None => {
            let Some(v) = pick_folder(&home().to_string_lossy(), "Where is your Obsidian vault? (Linen remembers it)")? else {
                return Ok(json!({ "cancelled": true }));
            };
            update_settings(&json!({ "vault_dir": v }))?;
            v
        }
    };
    let Some(dir) = pick_folder(&vault.to_string_lossy(), "Save to Obsidian — pick a folder")? else {
        return Ok(json!({ "cancelled": true }));
    };
    if !inside(&vault, &dir) {
        return Err("Pick a folder inside the Obsidian vault.".into());
    }
    save(&free_name(&dir, title, "md"), text, None, true)
}

/// The folders in ~/Projects, newest first, for "Save to project".
pub fn projects() -> Result<Value, String> {
    let root = projects_dir();
    let mut list: Vec<(u64, String)> = fs::read_dir(&root)
        .map_err(|e| format!("Can't read {}: {e}", root.display()))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .map(|n| (mtime(&root.join(&n)), n))
        .collect();
    list.sort_by_key(|b| std::cmp::Reverse(b.0));
    Ok(json!({ "root": root, "projects": list.into_iter().map(|(_, n)| n).collect::<Vec<_>>() }))
}

/// Save into ~/Projects/<project>/ under a free name; never overwrites.
pub fn save_to_project(project: &str, title: &str, ext: &str, text: &str) -> Result<Value, String> {
    if project.is_empty() || project.contains('/') || project.starts_with('.') {
        return Err("Pick a project folder.".into());
    }
    let dir = projects_dir().join(project);
    if !dir.is_dir() || !inside(&projects_dir(), &dir) {
        return Err(format!("{project} isn't a folder in Projects."));
    }
    save(&free_name(&dir, title, ext), text, None, true)
}

pub fn settings() -> Value {
    fs::read_to_string(config_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}

/// Merge `changes` into the saved settings.
pub fn update_settings(changes: &Value) -> Result<Value, String> {
    let mut s = settings();
    if let (Some(obj), Some(ch)) = (s.as_object_mut(), changes.as_object()) {
        for (k, v) in ch {
            obj.insert(k.clone(), v.clone());
        }
    }
    write_json(&config_path(), &s)?;
    Ok(s)
}

pub fn session() -> Value {
    fs::read_to_string(session_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

pub fn save_session(v: &Value) -> Result<(), String> {
    write_json(&session_path(), v)
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    // One write at a time, so two quick session saves can't trip over each other (review fix #3).
    static WRITING: Mutex<()> = Mutex::new(());
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = path.parent() {
        // Linen's own folders are private (0700); the session holds unsaved text, which may include passwords.
        fs::DirBuilder::new().recursive(true).mode(0o700).create(d).map_err(|e| e.to_string())?;
        let _ = fs::set_permissions(d, fs::Permissions::from_mode(0o700));
    }
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    let (tmp, mut f) = private_temp(path, 0o600).map_err(|e| e.to_string())?;
    let done = f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).and_then(|_| fs::rename(&tmp, path));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("linen-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_detects_outside_changes_and_keeps_mode() {
        let dir = temp("save");
        let p = dir.join("a.sh");
        fs::write(&p, "echo hi\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        let seen = mtime(&p);
        let r = save(&p, "echo two\n", Some(seen), false).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "echo two\n");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o755);
        let stale = r["mtime"].as_u64().unwrap() - 5000;
        assert_eq!(save(&p, "x", Some(stale), false).unwrap()["conflict"], true);
        assert_eq!(fs::read_to_string(&p).unwrap(), "echo two\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_files_are_private_and_never_overwrite() {
        let dir = temp("names");
        let a = free_name(&dir, "Router / passwords?", "txt");
        assert_eq!(a.file_name().unwrap(), "Router passwords.txt");
        save(&a, "one", None, true).unwrap();
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o600);
        let b = free_name(&dir, "Router / passwords?", "txt");
        assert_eq!(b.file_name().unwrap(), "Router passwords 2.txt");
        assert_eq!(free_name(&dir, "  ", "md").file_name().unwrap(), "Untitled.md");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn locking_replaces_the_plain_copy_and_reopens_with_the_password() {
        if Command::new("gpg").arg("--version").output().is_err() {
            return;
        }
        let dir = temp("lock");
        let plain = dir.join("Passwords.txt");
        save(&plain, "bank: 1234", None, true).unwrap();
        let r = lock(&plain.to_string_lossy(), "bank: 1234", "s3cret pass", "", "").unwrap();
        let locked = PathBuf::from(r["path"].as_str().unwrap());
        assert_eq!(locked.file_name().unwrap(), "Passwords.txt.gpg");
        assert!(!plain.exists());
        assert_eq!(fs::metadata(&locked).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(read(&locked).unwrap()["locked"], true);
        assert!(read(&locked).unwrap().get("text").is_none());
        assert!(unlock(&locked, "nope").is_err());
        let seen = read(&locked).unwrap()["mtime"].as_u64();
        save_locked(&locked, "bank: 5678", "s3cret pass", seen, false).unwrap();
        assert_eq!(unlock(&locked, "s3cret pass").unwrap()["text"], "bank: 5678");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_to_project_stays_inside_projects() {
        let root = temp("projects");
        fs::create_dir_all(root.join("care-log")).unwrap();
        unsafe { std::env::set_var("LINEN_PROJECTS", &root) };
        let r = save_to_project("care-log", "Notes", "txt", "hello").unwrap();
        assert!(r["path"].as_str().unwrap().ends_with("care-log/Notes.txt"));
        assert!(save_to_project("../etc", "x", "txt", "x").is_err());
        assert!(save_to_project("missing", "x", "txt", "x").is_err());
        assert_eq!(projects().unwrap()["projects"][0], "care-log");
        fs::remove_dir_all(root).unwrap();
    }

    // Review fix #3: saves never leave temp files behind, even when many run at once.
    #[test]
    fn overlapping_saves_use_private_unique_temp_files() {
        let dir = temp("race");
        let note = dir.join("note.txt");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let note = note.clone();
                std::thread::spawn(move || save(&note, &format!("version {i}"), None, true).unwrap())
            })
            .collect();
        handles.into_iter().for_each(|h| { h.join().unwrap(); });
        let names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["note.txt".to_string()], "no temp files left");
        assert_eq!(fs::metadata(&note).unwrap().permissions().mode() & 0o777, 0o600);
        let (tmp, _f) = private_temp(&note, 0o600).unwrap();
        assert_eq!(fs::metadata(&tmp).unwrap().permissions().mode() & 0o777, 0o600, "private from the first byte");
        fs::remove_dir_all(dir).unwrap();
    }

    // Review fix #7: plain text is never written over a locked note, whatever its name.
    #[test]
    fn plain_saves_refuse_a_locked_file_without_the_gpg_name() {
        let dir = temp("magic");
        let locked = dir.join("secrets.txt");
        fs::write(&locked, [0x8c, 0x0d, 0x04]).unwrap();
        assert!(save(&locked, "oops", None, true).is_err());
        assert_eq!(fs::read(&locked).unwrap(), vec![0x8c, 0x0d, 0x04]);
        fs::remove_dir_all(dir).unwrap();
    }

    // Review fix #7: locking a link locks and removes the real file, not just the link.
    #[test]
    fn locking_a_link_never_leaves_the_real_plain_copy() {
        if Command::new("gpg").arg("--version").output().is_err() {
            return;
        }
        let dir = temp("link");
        let real = dir.join("real.txt");
        let link = dir.join("link.txt");
        save(&real, "pin 4321", None, true).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let r = lock(&link.to_string_lossy(), "pin 4321", "pass phrase", "", "").unwrap();
        assert!(!real.exists(), "the real plain copy is gone");
        assert!(PathBuf::from(r["path"].as_str().unwrap()).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    // Review fix #13: a link inside Projects or the vault can't lead a save outside it.
    #[test]
    fn folder_checks_follow_links() {
        let root = temp("inside");
        let outside = temp("outside");
        fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sneaky")).unwrap();
        assert!(inside(&root, &root.join("real")));
        assert!(!inside(&root, &root.join("sneaky")));
        assert!(!inside(&root, &root.join("missing")));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    // Review fix #11: Linen never saves a note it couldn't open again.
    #[test]
    fn notes_over_the_open_limit_are_not_saved() {
        let dir = temp("big");
        let big = "x".repeat(MAX_BYTES as usize + 1);
        assert!(save(&dir.join("big.txt"), &big, None, true).is_err());
        assert!(!dir.join("big.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
