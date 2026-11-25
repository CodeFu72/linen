// 🔒 Lock: password-protected notes, using GPG's own format (AES-256), so a locked
// note can always be opened by hand with `gpg -d "note.txt.gpg"`, Linen or not.
// The password goes to gpg on the same pipe as the text, never on the command line,
// and --no-symkey-cache keeps gpg-agent from remembering it after Linen is done.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// A locked file: named .gpg, or starting with a GPG password-packet byte.
pub fn is_locked(path: &Path, head: &[u8]) -> bool {
    path.extension().is_some_and(|e| e == "gpg") || matches!(head.first(), Some(0x8c..=0x8f | 0xc3))
}

fn check(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Type a password.".into());
    }
    if password.contains(['\n', '\r']) {
        return Err("A password can't contain a line break.".into());
    }
    Ok(())
}

#[cfg(test)]
fn test_gnupg_home() -> String {
    use std::os::unix::fs::DirBuilderExt;
    static HOME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("linen-test-gnupg-{}", std::process::id()));
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        dir.to_string_lossy().into_owned()
    })
    .clone()
}

fn gpg(args: &[&str], password: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("gpg");
    // Tests use their own throwaway GPG folder, never the real ~/.gnupg (review fix #18).
    #[cfg(test)]
    command.args(["--homedir", &test_gnupg_home()]);
    let mut child = command
        .args(["--batch", "--quiet", "--no-tty", "--no-symkey-cache", "--pinentry-mode", "loopback", "--passphrase-fd", "0"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "GPG isn't installed, so notes can't be locked.".to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut input = Vec::with_capacity(password.len() + 1 + data.len());
    input.extend_from_slice(password.as_bytes());
    input.push(b'\n');
    input.extend_from_slice(data);
    // Write from a thread so a big note can't deadlock against gpg's output.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if out.status.success() {
        Ok(out.stdout)
    } else {
        // Keep it to gpg's last, most telling line; the full text is a wall of paths.
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("gpg failed").trim();
        Err(line.trim_start_matches("gpg: ").chars().take(160).collect())
    }
}

pub fn encrypt(text: &str, password: &str) -> Result<Vec<u8>, String> {
    check(password)?;
    gpg(&["--symmetric", "--cipher-algo", "AES256", "--output", "-"], password, text.as_bytes()).map_err(|e| format!("Couldn't lock: {e}"))
}

pub fn decrypt(bytes: &[u8], password: &str) -> Result<String, String> {
    check(password)?;
    let plain = gpg(&["--decrypt"], password, bytes).map_err(|e| {
        if e.contains("Bad session key") || e.contains("decryption failed") { "That password didn't work.".to_string() } else { format!("Couldn't unlock: {e}") }
    })?;
    String::from_utf8(plain).map_err(|_| "It unlocked, but it isn't plain text.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_and_unlocks_with_the_right_password_only() {
        if Command::new("gpg").arg("--version").output().is_err() {
            return;
        }
        let text = "Router: hunter2\nWi-Fi: ünïcode ✓\n";
        let locked = encrypt(text, "correct horse").unwrap();
        assert!(is_locked(Path::new("x.txt"), &locked));
        assert!(!locked.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(decrypt(&locked, "correct horse").unwrap(), text);
        assert_eq!(decrypt(&locked, "wrong").unwrap_err(), "That password didn't work.");
        assert!(encrypt(text, "").is_err());
        assert!(encrypt(text, "two\nlines").is_err());
    }
}
