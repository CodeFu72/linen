// Loopback-only HTTP server that feeds the WebKit UI. API calls must carry the
// per-launch token, so other local pages can't drive it. Adapted from Velvet.
// Review fixes (2026-11-24): every request must be addressed to 127.0.0.1:<port>
// (blocks DNS-rebinding pages), and the token is put into Linen's own window by
// main.rs instead of being handed out in the page (only `linen serve` still does).

use crate::{ai, files};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/app.css");
const JS: &str = include_str!("../ui/app.js");

fn font(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "Orbitron.ttf" => include_bytes!("../ui/fonts/Orbitron.ttf"),
        "Atkinson.ttf" => include_bytes!("../ui/fonts/AtkinsonHyperlegible-Regular.ttf"),
        "Atkinson-Bold.ttf" => include_bytes!("../ui/fonts/AtkinsonHyperlegible-Bold.ttf"),
        _ => return None,
    })
}

/// Files handed to Linen from the command line or a second launch, picked up by the UI.
pub static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Only `linen serve` (development in a normal browser) puts the token in the page.
pub static TOKEN_IN_PAGE: AtomicBool = AtomicBool::new(false);

pub fn start() -> (u16, String) {
    let server = Server::http("127.0.0.1:0").expect("bind loopback");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    let token = random_hex(16);
    let tok = token.clone();
    std::thread::spawn(move || {
        for rq in server.incoming_requests() {
            let tok = tok.clone();
            std::thread::spawn(move || handle(rq, &tok, port));
        }
    });
    (port, token)
}

/// Never falls back to zeros: a predictable token or an endless password loop is worse
/// than stopping. (At startup this stops Linen; in a request it fails just that request.)
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("the system random source (/dev/urandom) is unavailable");
    buf
}

fn random_hex(n: usize) -> String {
    random_bytes(n).iter().map(|b| format!("{b:02x}")).collect()
}

/// A strong password from the system's random source: letters, digits and a few
/// symbols that are easy to type, with no look-alikes (0/O, 1/l/I).
pub fn password(len: usize) -> String {
    const SET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^*-_=+";
    let len = len.clamp(12, 64);
    loop {
        let pw: String = random_bytes(len * 2).iter().filter(|b| (**b as usize) < 256 - 256 % SET.len()).take(len).map(|b| SET[*b as usize % SET.len()] as char).collect();
        let has = |f: fn(&char) -> bool| pw.chars().any(|c| f(&c));
        if pw.len() == len && has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase) && has(char::is_ascii_digit) && has(|c| !c.is_ascii_alphanumeric()) {
            return pw;
        }
    }
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("header")
}

fn send(rq: Request, status: u16, ctype: &str, body: Vec<u8>) {
    let resp = Response::from_data(body)
        .with_status_code(status)
        .with_header(header("Content-Type", ctype))
        .with_header(header("Cache-Control", "no-store"));
    let _ = rq.respond(resp);
}

fn serve_static(rq: Request, path: &str, token: &str) {
    match path {
        "/" | "/index.html" => {
            let shown = if TOKEN_IN_PAGE.load(Ordering::Relaxed) { token } else { "" };
            send(rq, 200, "text/html; charset=utf-8", INDEX.replace("{{TOKEN}}", shown).into_bytes())
        }
        "/app.css" => send(rq, 200, "text/css; charset=utf-8", CSS.as_bytes().to_vec()),
        "/app.js" => send(rq, 200, "text/javascript; charset=utf-8", JS.as_bytes().to_vec()),
        p if p.starts_with("/fonts/") => match font(&p[7..]) {
            Some(b) => send(rq, 200, "font/ttf", b.to_vec()),
            None => send(rq, 404, "text/plain", b"missing".to_vec()),
        },
        _ => send(rq, 404, "text/plain", b"not found".to_vec()),
    }
}

/// Text cleanups come from Rinse (`rinse run OP`), so both apps behave the same.
fn rinse(op: &str, text: &str) -> Result<Value, String> {
    if op.is_empty() || !op.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()) {
        return Err("Unknown cleanup.".into());
    }
    let mut child = Command::new("rinse")
        .args(["run", op])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "Rinse isn't installed, so cleanups aren't available.".to_string())?;
    // Write from a thread so a long note can't deadlock against rinse's output (pipe buffers are ~64 KB).
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let input = text.as_bytes().to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    writer.join().map_err(|_| "Rinse input failed".to_string())?.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(json!({ "text": String::from_utf8_lossy(&out.stdout) }))
}

/// True only for requests addressed to this server itself (127.0.0.1 or localhost on our port).
fn host_ok(rq: &Request, port: u16) -> bool {
    let host = rq.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().to_ascii_lowercase());
    matches!(host, Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

fn handle(mut rq: Request, token: &str, port: u16) {
    if !host_ok(&rq, port) {
        return send(rq, 403, "text/plain", b"forbidden".to_vec());
    }
    let path = rq.url().split('?').next().unwrap_or("/").to_string();
    if !path.starts_with("/api/") {
        return serve_static(rq, &path, token);
    }
    let authed = rq.headers().iter().any(|h| h.field.equiv("X-Linen-Token") && h.value.as_str() == token);
    if !authed {
        return send(rq, 403, "application/json", br#"{"error":"forbidden"}"#.to_vec());
    }
    const MAX_BODY: u64 = 24_000_000;
    let mut body = String::new();
    if rq.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() {
        return send(rq, 400, "application/json", br#"{"error":"That request couldn't be read."}"#.to_vec());
    }
    if body.len() as u64 > MAX_BODY {
        return send(rq, 413, "application/json", br#"{"error":"That's too big to send to Linen's server."}"#.to_vec());
    }
    // A GET has no body; a POST must be real JSON, never quietly read as "nothing" (review fix #11).
    let input: Value = if body.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => return send(rq, 400, "application/json", br#"{"error":"That request was damaged, so nothing changed."}"#.to_vec()),
        }
    };
    let s = |k: &str| input[k].as_str().unwrap_or_default().to_string();

    let result: Result<Value, String> = match (rq.method(), path.as_str()) {
        (Method::Get, "/api/state") => Ok(json!({
            "settings": files::settings(), "session": files::session(),
            "home": files::home(), "vault": files::vault_dir(), "projects_dir": files::projects_dir(),
            "omarchy_theme": crate::theme::current(),
            "ai_tools": ai::TOOLS,
        })),
        (Method::Get, "/api/pending") => Ok(json!({ "files": pending() })),
        (Method::Post, "/api/open_dialog") => {
            files::pick_files(&s("dir")).map(|picked| json!({ "files": picked.iter().map(|p| files::read(p).unwrap_or_else(|e| json!({ "error": e }))).collect::<Vec<_>>() }))
        }
        (Method::Post, "/api/open_path") => files::read(&PathBuf::from(s("path"))),
        (Method::Post, "/api/save") => {
            let (p, mt, force) = (PathBuf::from(s("path")), input["mtime"].as_u64(), input["force"].as_bool().unwrap_or(false));
            match input["password"].as_str() {
                Some(pw) => files::save_locked(&p, &s("text"), pw, mt, force),
                None => files::save(&p, &s("text"), mt, force),
            }
        }
        (Method::Post, "/api/unlock") => files::unlock(&PathBuf::from(s("path")), &s("password")),
        (Method::Post, "/api/lock") => files::lock(&s("path"), &s("text"), &s("password"), &s("dir"), &s("name")),
        (Method::Post, "/api/save_as") => match files::pick_save(&s("dir"), &s("name")) {
            Ok(Some(p)) => files::save(&p, &s("text"), None, true),
            Ok(None) => Ok(json!({ "cancelled": true })),
            Err(e) => Err(e),
        },
        (Method::Post, "/api/save_vault") => files::save_to_vault(&s("text"), &s("title")),
        (Method::Get, "/api/projects") => files::projects(),
        (Method::Post, "/api/save_project") => files::save_to_project(&s("project"), &s("title"), &s("ext"), &s("text")),
        (Method::Post, "/api/clean") => rinse(&s("op"), &s("text")),
        (Method::Post, "/api/password") => Ok(json!({ "password": password(input["length"].as_u64().unwrap_or(20) as usize) })),
        (Method::Post, "/api/ai/start") => {
            let model = files::settings()["ai_model"].as_str().unwrap_or(ai::DEFAULT_MODEL).to_string();
            ai::start(&s("tool"), &s("text"), &model).map(|id| json!({ "id": id }))
        }
        (Method::Post, "/api/ai/poll") => ai::poll(input["id"].as_u64().unwrap_or_default()),
        (Method::Post, "/api/ai/cancel") => {
            ai::cancel(input["id"].as_u64().unwrap_or_default());
            Ok(json!({ "ok": true }))
        }
        (Method::Post, "/api/settings") => files::update_settings(&input),
        (Method::Post, "/api/session") => files::save_session(&input).map(|_| json!({ "ok": true })),
        _ => Err("unknown request".into()),
    };
    match result {
        Ok(v) => send(rq, 200, "application/json", v.to_string().into_bytes()),
        Err(e) => send(rq, 400, "application/json", json!({ "error": e }).to_string().into_bytes()),
    }
}

fn pending() -> Vec<Value> {
    let paths: Vec<PathBuf> = std::mem::take(&mut *PENDING.lock().unwrap());
    paths.iter().map(|p| files::read(p).unwrap_or_else(|e| json!({ "error": e }))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_strong_and_varied() {
        let a = password(20);
        let b = password(20);
        assert_eq!(a.chars().count(), 20);
        assert_ne!(a, b);
        assert!(!a.chars().any(|c| "0O1lI ".contains(c)));
        assert_eq!(password(3).len(), 12);
    }

    #[test]
    fn cleanups_go_through_rinse_and_refuse_odd_names() {
        assert!(rinse("../../bin/sh", "x").is_err());
        if Command::new("rinse").arg("ops").output().is_ok() {
            assert_eq!(rinse("upper_case", "hello").unwrap()["text"], "HELLO");
        }
    }

    // Review fix #1: only requests addressed to Linen itself get an answer, and the
    // page never carries the token (outside `linen serve`).
    #[test]
    fn requests_must_be_addressed_to_linen_and_the_page_hides_the_token() {
        use std::io::{Read, Write};
        let (port, token) = start();
        let ask = |host: &str, path: &str, tok: Option<&str>| {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            let extra = tok.map(|t| format!("X-Linen-Token: {t}\r\n")).unwrap_or_default();
            write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n").unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        let me = format!("127.0.0.1:{port}");
        assert!(ask(&format!("evil.example:{port}"), "/", None).starts_with("HTTP/1.1 403"));
        assert!(ask(&format!("evil.example:{port}"), "/api/state", Some(&token)).starts_with("HTTP/1.1 403"));
        let page = ask(&me, "/", None);
        assert!(page.starts_with("HTTP/1.1 200") && !page.contains(&token));
        assert!(ask(&me, "/api/state", Some("wrong")).starts_with("HTTP/1.1 403"));
        assert!(ask(&me, "/api/state", Some(&token)).starts_with("HTTP/1.1 200"));
    }

    // Review fix #10: a long note can't deadlock the cleanup pipe.
    #[test]
    fn long_notes_go_through_rinse_without_deadlock() {
        if Command::new("rinse").arg("ops").output().is_err() {
            return;
        }
        let text = "word ".repeat(60_000);
        assert_eq!(rinse("upper_case", &text).unwrap()["text"].as_str().unwrap().len(), text.len());
    }

    // Review fix #11: a damaged request is refused, never read as "nothing".
    #[test]
    fn damaged_requests_change_nothing() {
        use std::io::{Read, Write};
        let (port, token) = start();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = "{not json";
        write!(s, "POST /api/session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Linen-Token: {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 400"), "{out}");
    }
}
