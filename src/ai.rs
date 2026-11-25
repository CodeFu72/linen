#![allow(clippy::collapsible_if)] // Same code as Rinse; kept identical on purpose.
// Offline AI rewrites through the local Ollama server (shared with Rinse and FormForge):
// 127.0.0.1 only, cloud models disabled, one model loaded at a time. Each request runs
// as a background job the UI polls, so text appears as it's written and can be stopped.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "http://127.0.0.1:11434";
pub const DEFAULT_MODEL: &str = "qwen3:4b-instruct";
const MAX_INPUT_CHARS: usize = 16_000;

#[derive(Serialize, Clone, Copy)]
pub struct AiTool {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    #[serde(skip)]
    instruction: &'static str,
}

const fn tool(id: &'static str, label: &'static str, hint: &'static str, instruction: &'static str) -> AiTool {
    AiTool { id, label, hint, instruction }
}

pub const TOOLS: &[AiTool] = &[
    tool("ai_grammar", "Fix grammar & spelling", "Typos, grammar and punctuation; your words otherwise stay",
        "Proofread it like a careful copy editor: fix spelling, grammar, punctuation and capitalization (capitalize the first word of each sentence, the word I, and names of people and places), subject-verb agreement, and commonly confused words (there/their/they're, your/you're, its/it's). Keep the author's own words, tone, paragraphs and line breaks; change only what is wrong."),
    tool("ai_professional", "Make it professional", "Clear, polite business writing for a client",
        "Rewrite it as clear, polite, professional business writing, suitable for an email to a client. Keep it about the same length."),
    tool("ai_friendly", "Make it friendly", "Warm and casual, like a note to an old friend",
        "Rewrite it in a warm, friendly, casual tone, like a note to a longtime friend and colleague. Keep it about the same length."),
    tool("ai_shorter", "Make it shorter", "Tighter, every important fact kept",
        "Make it noticeably shorter and more direct. Keep every important fact, name, number and request."),
    tool("ai_bullets", "Turn into bullets", "A tidy Markdown list, one idea per line",
        "Turn it into a concise Markdown bulleted list using '- ', one idea per bullet. Keep every important fact."),
    tool("ai_summary", "Summarize", "The gist in 2–4 sentences",
        "Summarize it in 2 to 4 plain sentences."),
    tool("ai_synonyms", "Synonyms", "Other words that fit here",
        "The text gives a WORD and the SENTENCE it appears in. List up to 8 synonyms for the word that would fit naturally in that sentence, simplest first. Output only the synonyms separated by commas, nothing else."),
    tool("ai_obsidian", "Notes → Obsidian", "Title, sections, bullets and to-do checkboxes",
        "Turn these rough notes into a tidy Obsidian Markdown note: a '# ' title, '## ' sections where they help, '- ' bullets, and '- [ ] ' checkboxes for action items and to-dos."),
];

const RULES: &str = "You are a careful text editor. Apply the instruction to the user's text.\n\
Output ONLY the resulting text: no introduction, no explanation, no notes, no quotation marks around it, no code fences.\n\
Keep names, numbers, dates, prices, URLs and email addresses exactly as written.\n\
Never add facts, details or promises that are not in the text.\n\
Write in the same language as the text.";

// ---------- jobs ----------

#[derive(Default)]
struct Job {
    text: String,
    status: String,
    done: bool,
    error: Option<String>,
    child: Option<Child>,
    cancelled: bool,
}

static JOBS: LazyLock<Mutex<HashMap<u64, Arc<Mutex<Job>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: Mutex<u64> = Mutex::new(1);

pub fn start(tool_id: &str, text: &str, model: &str) -> Result<u64, String> {
    let tool = TOOLS.iter().find(|t| t.id == tool_id).ok_or_else(|| format!("unknown AI tool {tool_id}"))?;
    check_model_name(model)?;
    if text.trim().is_empty() {
        return Err("Nothing to rewrite yet.".into());
    }
    if text.chars().count() > MAX_INPUT_CHARS {
        return Err(format!("That's a lot of text for the local AI. Select up to {MAX_INPUT_CHARS} characters and try again."));
    }
    let id = {
        let mut n = NEXT_ID.lock().unwrap();
        *n += 1;
        *n
    };
    let job = Arc::new(Mutex::new(Job { status: "Starting the local AI…".into(), ..Default::default() }));
    JOBS.lock().unwrap().insert(id, job.clone());
    let (instruction, text, model) = (tool.instruction.to_string(), text.to_string(), model.to_string());
    std::thread::spawn(move || {
        let result = run(&job, &instruction, &text, &model);
        let mut j = job.lock().unwrap();
        j.child = None;
        j.done = true;
        if let Err(e) = result {
            if !j.cancelled {
                j.error = Some(e);
            }
        } else {
            j.text = tidy(&j.text, &text);
        }
    });
    Ok(id)
}

pub fn poll(id: u64) -> Result<Value, String> {
    let job = JOBS.lock().unwrap().get(&id).cloned().ok_or("That AI job has ended.")?;
    let j = job.lock().unwrap();
    let reply = json!({ "text": visible(&j.text), "status": j.status, "done": j.done, "error": j.error });
    if j.done {
        drop(j);
        JOBS.lock().unwrap().remove(&id);
    }
    Ok(reply)
}

pub fn cancel(id: u64) {
    if let Some(job) = JOBS.lock().unwrap().remove(&id) {
        let mut j = job.lock().unwrap();
        j.cancelled = true;
        if let Some(mut c) = j.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn set_status(job: &Mutex<Job>, s: &str) {
    job.lock().unwrap().status = s.to_string();
}

fn run(job: &Mutex<Job>, instruction: &str, text: &str, model: &str) -> Result<(), String> {
    // Stop is honored before the slow steps, too (review fix #16).
    let stopped = || job.lock().map(|j| j.cancelled).unwrap_or(true);
    if stopped() {
        return Err("cancelled".into());
    }
    ensure_server(job)?;
    if stopped() {
        return Err("cancelled".into());
    }
    set_status(job, "Checking the model…");
    verify_local_model(model)?;
    set_status(job, &format!("Loading {model}… (first run takes a little while)"));
    let body = json!({
        "model": model,
        "stream": true,
        "think": false,
        "keep_alive": "10m",
        "options": { "temperature": 0.2, "num_ctx": 8192 },
        "messages": [
            { "role": "system", "content": format!("{RULES}\n\nInstruction: {instruction}") },
            { "role": "user", "content": text },
        ],
    });
    let mut child = Command::new("curl")
        .args(["-sS", "-N", "--fail-with-body", "-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-"])
        .arg(format!("{HOST}/api/chat"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    child.stdin.take().unwrap().write_all(body.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().unwrap();
    {
        let mut j = job.lock().unwrap();
        if j.cancelled {
            let _ = child.kill();
            return Err("cancelled".into());
        }
        j.child = Some(child);
    }
    let mut first = true;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).map_err(|_| format!("Unexpected reply from Ollama: {line}"))?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("Ollama: {e}"));
        }
        let mut j = job.lock().unwrap();
        if first {
            j.status = "Writing…".into();
            first = false;
        }
        if let Some(piece) = v["message"]["content"].as_str() {
            j.text.push_str(piece);
        }
        if v["done"].as_bool() == Some(true) {
            j.status = "Done".into();
            break;
        }
    }
    let child = job.lock().unwrap().child.take();
    if let Some(mut c) = child {
        let status = c.wait().map_err(|e| e.to_string())?;
        if !status.success() && !job.lock().unwrap().cancelled {
            let mut err = String::new();
            if let Some(mut e) = c.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            return Err(format!("The local AI stopped unexpectedly. {}", err.trim()));
        }
    }
    Ok(())
}

// ---------- server ----------

fn get(path: &str) -> Option<Value> {
    let out = Command::new("curl").args(["-s", "-m", "3"]).arg(format!("{HOST}{path}")).output().ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

fn post(path: &str, body: &Value) -> Option<Value> {
    let out = Command::new("curl")
        .args(["-s", "-m", "10", "-X", "POST", "-H", "Content-Type: application/json", "-d"])
        .arg(body.to_string())
        .arg(format!("{HOST}{path}"))
        .output()
        .ok()?;
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

fn ollama_bin() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bundled = home.join(".local/share/formforge/ollama-runtime/bin/ollama");
    if bundled.is_file() {
        return Some(bundled);
    }
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| PathBuf::from(d).join("ollama")).find(|p| p.is_file())
}

/// Same local-only switch FormForge sets: ~/.ollama/server.json disable_ollama_cloud = true.
fn ensure_local_only_config() -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let p = home.join(".ollama/server.json");
    let mut data: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if data["disable_ollama_cloud"] == json!(true) {
        return Ok(());
    }
    data["disable_ollama_cloud"] = json!(true);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&p, serde_json::to_string_pretty(&data).unwrap() + "\n").map_err(|e| e.to_string())
}

fn ensure_server(job: &Mutex<Job>) -> Result<(), String> {
    ensure_local_only_config()?;
    if get("/api/version").is_some() {
        return Ok(());
    }
    set_status(job, "Starting the local AI server…");
    let bin = ollama_bin().ok_or("Ollama isn't installed, so the AI helpers are off. On Omarchy: omarchy pkg add ollama")?;
    let log_dir = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/linen");
    let _ = std::fs::create_dir_all(&log_dir);
    // Private log (review fix #16).
    let log = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(log_dir.join("ollama.log")).map_err(|e| e.to_string())?
    };
    Command::new(bin)
        .arg("serve")
        .env("OLLAMA_HOST", "127.0.0.1:11434")
        .env("OLLAMA_NO_CLOUD", "1")
        .env("OLLAMA_NUM_PARALLEL", "1")
        .env("OLLAMA_MAX_LOADED_MODELS", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| format!("Couldn't start Ollama: {e}"))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        if get("/api/version").is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err("The local AI server didn't start. See ~/.local/state/linen/ollama.log".into())
}

fn check_model_name(model: &str) -> Result<(), String> {
    let ok = !model.is_empty()
        && model.len() <= 100
        && model.chars().all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
        && !model.to_lowercase().contains("cloud");
    if ok { Ok(()) } else { Err("Choose an installed local model; cloud models are disabled.".into()) }
}

/// Installed, local-weights model only (no remote/cloud-backed models).
fn verify_local_model(model: &str) -> Result<(), String> {
    let tags = get("/api/tags").ok_or("Couldn't reach the local AI server.")?;
    let installed: Vec<&str> = tags["models"].as_array().map(|m| m.iter().filter_map(|x| x["name"].as_str()).collect()).unwrap_or_default();
    if !installed.contains(&model) {
        return Err(format!("The model {model} isn't installed. Run `ollama pull {model}` in a terminal."));
    }
    let info = post("/api/show", &json!({ "model": model })).ok_or("Couldn't read the model details.")?;
    if !info["remote_host"].is_null() || !info["remote_model"].is_null() || info["model_info"].is_null() {
        return Err("Remote-backed models are disabled. Choose a model with local weights.".into());
    }
    Ok(())
}

#[allow(dead_code)] // Kept for a future model picker, as in Rinse.
pub fn models() -> Value {
    let list: Vec<String> = get("/api/tags")
        .and_then(|t| t["models"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["name"].as_str().map(String::from))
        .filter(|n| check_model_name(n).is_ok())
        .collect();
    json!({ "running": get("/api/version").is_some(), "models": list })
}

// ---------- output cleanup ----------

/// Hide a <think>…</think> block while it streams (some models think out loud).
fn visible(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end) => s.replace_range(start..start + end + "</think>".len(), ""),
            None => s.truncate(start),
        }
    }
    s.trim_start().to_string()
}

/// Final tidy: drop thinking, a wrapping code fence or quotes, a "Here is…" preamble;
/// match the original's trailing newline.
fn tidy(raw: &str, original: &str) -> String {
    let mut s = visible(raw).trim().to_string();
    if s.starts_with("```") && s.ends_with("```") && s.len() > 6 {
        s = s.trim_start_matches('`').trim_end_matches('`').to_string();
        if let Some(nl) = s.find('\n') {
            if !s[..nl].contains(' ') {
                s = s[nl + 1..].to_string();
            }
        }
        s = s.trim().to_string();
    }
    let lower = s.to_lowercase();
    if (lower.starts_with("here is") || lower.starts_with("here's") || lower.starts_with("sure")) && s.contains('\n') {
        if let Some(nl) = s.find('\n') {
            if s[..nl].trim_end().ends_with(':') {
                s = s[nl + 1..].trim_start().to_string();
            }
        }
    }
    let wrapped = |a: char, b: char| s.starts_with(a) && s.ends_with(b) && s.chars().count() > 2 && !original.trim_start().starts_with(a);
    if wrapped('"', '"') || wrapped('“', '”') {
        let mut cs = s.chars();
        cs.next();
        cs.next_back();
        s = cs.as_str().to_string();
    }
    // models like Markdown hard breaks ("  " at line ends); drop them
    s = s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    if original.ends_with('\n') && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_output() {
        assert_eq!(tidy("<think>hmm</think>\n\nHello there.", "hi"), "Hello there.");
        assert_eq!(tidy("```markdown\n# Title\n- a\n```", "x"), "# Title\n- a");
        assert_eq!(tidy("Here is the revised text:\nHello.", "x"), "Hello.");
        assert_eq!(tidy("\"Hello.\"", "hello"), "Hello.");
        assert_eq!(tidy("\"Quoted\" stays", "\"quoted\" stays"), "\"Quoted\" stays");
        assert_eq!(tidy("Hi", "hi\n"), "Hi\n");
        assert_eq!(tidy("Hello,  \n\nThanks.  ", "x"), "Hello,\n\nThanks.");
        assert_eq!(visible("<think>partial"), "");
    }

    #[test]
    fn model_names() {
        assert!(check_model_name("qwen3:4b").is_ok());
        assert!(check_model_name("gpt-oss:120b-cloud").is_err());
        assert!(check_model_name("../../etc").is_err());
    }
}
