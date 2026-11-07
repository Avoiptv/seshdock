use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Serialize, Clone, Debug)]
pub struct Session {
    pub harness: &'static str,
    pub id: String,
    pub title: String,
    pub updated: i64, // ms since epoch
    pub source: Option<String>, // e.g. "VS Code" for Claude Code extension sessions
}

#[derive(Serialize, Debug)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub description: String,
    pub exists: bool,
    pub vault: bool, // has .obsidian → can open in Obsidian
    pub last_active: i64,
    pub sessions: Vec<Session>,
}

type Found = Vec<(String, Session)>;

fn session(harness: &'static str, id: String, title: Option<String>, updated: i64) -> Session {
    let title = title.map(|t| clip(&t)).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled session".into());
    Session { harness, id, title, updated, source: None }
}

fn env_dir(var: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or(default)
}

/// First LINE_CAP lines of a text file (empty if unreadable).
fn lines(p: &Path) -> impl Iterator<Item = String> {
    fs::File::open(p).into_iter().flat_map(|f| BufReader::new(f).lines().take(LINE_CAP).map_while(Result::ok))
}

/// All files with `ext` under `root`, recursively.
fn walk(root: &Path, ext: &str) -> Vec<PathBuf> {
    let (mut out, mut stack) = (Vec::new(), vec![root.to_path_buf()]);
    while let Some(dir) = stack.pop() {
        for p in read_dir(&dir) {
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == ext) {
                out.push(p);
            }
        }
    }
    out
}

// Max lines read per session file; titles and cwd live near the top.
const LINE_CAP: usize = 300;

pub fn scan_all() -> Vec<Project> {
    let home = dirs::home_dir().unwrap_or_default();
    let scanners: [fn(&Path) -> Found; 9] = [claude, codex, opencode, pi, antigravity, dsh, deepseek, openclaw, hermes];
    let found: Found = std::thread::scope(|s| {
        let handles: Vec<_> = scanners.iter().map(|f| s.spawn(|| f(&home))).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });

    let mut by_key: HashMap<String, (String, Vec<Session>)> = HashMap::new();
    for (cwd, s) in found {
        let entry = by_key.entry(path_key(&cwd)).or_insert_with(|| (native(&cwd), Vec::new()));
        entry.1.push(s);
    }

    let mut projects: Vec<Project> = by_key
        .into_values()
        .map(|(path, mut sessions)| {
            sessions.sort_by(|a, b| b.updated.cmp(&a.updated));
            let p = Path::new(&path);
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            let description = describe(p).unwrap_or_else(|| sessions[0].title.clone());
            Project { exists: p.is_dir(), vault: p.join(".obsidian").is_dir(), last_active: sessions[0].updated, name, description, path, sessions }
        })
        .collect();
    projects.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    projects
}

/// Merge key: same folder written as `C:\x`, `C:/x` or `c:\x\` must collide.
fn path_key(p: &str) -> String {
    let k = p.replace('\\', "/").trim_end_matches('/').to_string();
    if cfg!(windows) { k.to_lowercase() } else { k }
}

fn native(p: &str) -> String {
    if !cfg!(windows) { return p.to_string(); }
    let p = p.replace('/', "\\");
    // `c:\x` → `C:\x`
    match p.as_bytes() {
        [d, b':', ..] => format!("{}{}", (*d as char).to_ascii_uppercase(), &p[1..]),
        _ => p,
    }
}

/// Markdown → plain text for one README line: `[text](url)` → `text`, drop `**` and backticks.
fn plain(md: &str) -> String {
    let mut out = String::new();
    let mut rest = md;
    while let Some(i) = rest.find("](") {
        let (head, tail) = rest.split_at(i);
        match head.rfind('[') {
            Some(k) => { out.push_str(&head[..k]); out.push_str(&head[k + 1..]); }
            None => out.push_str(head),
        }
        rest = tail.find(')').map_or("", |j| &tail[j + 1..]);
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

fn mtime_ms(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clip(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 120 { s.chars().take(117).collect::<String>() + "…" } else { s }
}

// ---------- Claude Code (CLI, VS Code / JetBrains extensions, Desktop) ----------

fn claude(home: &Path) -> Found {
    let root = env_dir("CLAUDE_CONFIG_DIR", home.join(".claude"));
    let mut out = Vec::new();
    for dir in read_dir(&root.join("projects")) {
        for f in read_dir(&dir) {
            if f.extension().is_none_or(|e| e != "jsonl") { continue; }
            if let Some((cwd, title, source)) = parse_claude(lines(&f)) {
                let id = f.file_stem().unwrap().to_string_lossy().into_owned();
                let mut s = session("claude", id, title, mtime_ms(&f));
                s.source = source;
                out.push((cwd, s));
            }
        }
    }
    out
}

/// -> (cwd, title, source)
fn parse_claude(lines: impl Iterator<Item = String>) -> Option<(String, Option<String>, Option<String>)> {
    let (mut cwd, mut title, mut first_msg, mut entry) = (None, None, None, None);
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if cwd.is_none() {
            cwd = v["cwd"].as_str().map(String::from);
        }
        if entry.is_none() {
            entry = v["entrypoint"].as_str().map(String::from);
        }
        for k in ["customTitle", "aiTitle", "summary"] {
            if let Some(t) = v[k].as_str() {
                title = Some(t.to_string());
            }
        }
        if first_msg.is_none() && v["type"] == "user" && v["isMeta"] != true {
            first_msg = user_text(&v["message"]["content"]);
        }
        if cwd.is_some() && title.is_some() && entry.is_some() { break; }
    }
    let source = match entry.as_deref() {
        None | Some("cli") => None,
        Some("claude-vscode") => Some("VS Code".to_string()),
        Some("claude-desktop") => Some("Desktop".to_string()),
        Some(e) if e.starts_with("sdk") => Some("SDK".to_string()),
        Some(e) => Some(e.trim_start_matches("claude-").to_string()),
    };
    Some((cwd?, title.or(first_msg), source))
}

/// Text of a user message, skipping injected `<command…>`/`<system…>` blocks.
fn user_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => return None,
    };
    let t = text.trim();
    (!t.is_empty() && !t.starts_with('<')).then(|| t.to_string())
}

// ---------- Codex ----------

fn codex(home: &Path) -> Found {
    let root = env_dir("CODEX_HOME", home.join(".codex"));
    let names = codex_names(&root.join("session_index.jsonl"));
    walk(&root.join("sessions"), "jsonl")
        .into_iter()
        .filter_map(|p| {
            let (cwd, id, title) = parse_codex(lines(&p))?;
            let title = names.get(&id).cloned().or(title);
            Some((cwd, session("codex", id, title, mtime_ms(&p))))
        })
        .collect()
}

fn codex_names(index: &Path) -> HashMap<String, String> {
    let Ok(f) = fs::File::open(index) else { return HashMap::new() };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .filter_map(|v| Some((v["id"].as_str()?.to_string(), v["thread_name"].as_str()?.to_string())))
        .collect()
}

/// -> (cwd, id, first user message). None for sub-agent threads (guardian etc).
fn parse_codex(mut lines: impl Iterator<Item = String>) -> Option<(String, String, Option<String>)> {
    let meta: Value = serde_json::from_str(&lines.next()?).ok()?;
    let p = &meta["payload"];
    if meta["type"] != "session_meta" || p["source"].is_object() { return None; }
    let (cwd, id) = (p["cwd"].as_str()?.to_string(), p["id"].as_str()?.to_string());
    let title = lines
        .filter(|l| l.contains("\"user_message\""))
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .find_map(|v| v["payload"]["message"].as_str().map(String::from));
    Some((cwd, id, title))
}

// ---------- OpenCode ----------

fn sqlite(db: &Path) -> Option<rusqlite::Connection> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    db.is_file().then(|| rusqlite::Connection::open_with_flags(db, flags).ok()).flatten()
}

fn opencode(home: &Path) -> Found {
    let db = env_dir("XDG_DATA_HOME", home.join(".local/share")).join("opencode/opencode.db");
    sqlite(&db).and_then(|c| query_opencode(&c).ok()).unwrap_or_default()
}

fn query_opencode(conn: &rusqlite::Connection) -> rusqlite::Result<Found> {
    let mut st = conn.prepare(
        "SELECT id, directory, title, time_updated FROM session
         WHERE parent_id IS NULL AND time_archived IS NULL",
    )?;
    let rows = st.query_map([], |r| {
        let title: String = r.get(2)?;
        let title = (!title.starts_with("New session - ")).then_some(title);
        Ok((r.get(1)?, session("opencode", r.get(0)?, title, r.get(3)?)))
    })?;
    rows.collect()
}

// ---------- Pi (pi.dev coding agent) ----------

fn pi(home: &Path) -> Found {
    walk(&home.join(".pi/agent/sessions"), "jsonl")
        .into_iter()
        .filter_map(|p| {
            let (cwd, id, title) = parse_pi(lines(&p))?;
            Some((cwd, session("pi", id, title, mtime_ms(&p))))
        })
        .collect()
}

/// Line 1 is `{"type":"session","id","cwd"}`; title = session name or first user message.
fn parse_pi(mut lines: impl Iterator<Item = String>) -> Option<(String, String, Option<String>)> {
    let head: Value = serde_json::from_str(&lines.next()?).ok()?;
    if head["type"] != "session" { return None; }
    let (cwd, id) = (head["cwd"].as_str()?.to_string(), head["id"].as_str()?.to_string());
    let (mut name, mut first) = (None, None);
    for l in lines {
        if !l.contains("session_info") && (first.is_some() || !l.contains("\"role\":\"user\"")) { continue; }
        let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
        if v["type"] == "session_info" {
            name = v["name"].as_str().map(String::from).or(name);
        } else if first.is_none() && v["message"]["role"] == "user" {
            first = user_text(&v["message"]["content"]);
        }
    }
    Some((cwd, id, name.or(first)))
}

// ---------- Antigravity (Google) ----------

fn antigravity(home: &Path) -> Found {
    let root = home.join(".gemini/antigravity");
    let mut out = Vec::new();
    // ponytail: legacy *.pb conversations are encrypted (no readable workspace path) → skipped
    for db in read_dir(&root.join("conversations")) {
        if db.extension().is_none_or(|e| e != "db") { continue; }
        let id = db.file_stem().unwrap().to_string_lossy().into_owned();
        let blob: Option<Vec<u8>> =
            sqlite(&db).and_then(|c| c.query_row("SELECT data FROM trajectory_metadata_blob", [], |r| r.get(0)).ok());
        let Some(cwd) = blob.as_deref().and_then(file_uri_path) else { continue };
        let title = fs::read_to_string(root.join("annotations").join(format!("{id}.pbtxt")))
            .ok()
            .and_then(|t| Some(t.split("title:\"").nth(1)?.split('"').next()?.to_string()));
        let updated = mtime_ms(&db).max(mtime_ms(&db.with_extension("db-wal")));
        out.push((cwd, session("antigravity", id, title, updated)));
    }
    out
}

// ---------- DeepSeek TUI / Codewhale ----------

fn deepseek(home: &Path) -> Found {
    let homes = [env_dir("CODEWHALE_HOME", home.join(".codewhale")), env_dir("DEEPSEEK_HOME", home.join(".deepseek"))];
    let mut seen = std::collections::HashSet::new(); // legacy home may still hold migrated copies
    homes
        .iter()
        .flat_map(|h| read_dir(&h.join("sessions")))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let v: Value = serde_json::from_reader(BufReader::new(fs::File::open(&p).ok()?)).ok()?;
            let (cwd, id, title) = parse_deepseek(&v)?;
            seen.insert(id.clone()).then(|| (cwd, session("deepseek", id, title, mtime_ms(&p))))
        })
        .collect()
}

/// `SavedSession.metadata { id, title, workspace }`
fn parse_deepseek(v: &Value) -> Option<(String, String, Option<String>)> {
    let m = &v["metadata"];
    Some((m["workspace"].as_str()?.to_string(), m["id"].as_str()?.to_string(), m["title"].as_str().map(String::from)))
}

// ---------- DeepSeek Harness (official desktop app) ----------

fn dsh(home: &Path) -> Found {
    walk(&home.join(".dsh/sessions"), "zstd")
        .into_iter()
        .filter_map(|p| {
            let dec = ruzstd::decoding::StreamingDecoder::new(BufReader::new(fs::File::open(&p).ok()?)).ok()?;
            let (cwd, id, title) = parse_dsh(BufReader::new(dec).lines().take(LINE_CAP).map_while(Result::ok))?;
            Some((cwd, session("dsh", id, title, mtime_ms(&p))))
        })
        .collect()
}

/// `session.v4.jsonl` (zstd): line 1 `{"type":"session","id","cwd","delegationDepth"}`,
/// then `session/title` events; the last one is the LLM-generated title.
fn parse_dsh(mut lines: impl Iterator<Item = String>) -> Option<(String, String, Option<String>)> {
    let head: Value = serde_json::from_str(&lines.next()?).ok()?;
    if head["type"] != "session" || head["delegationDepth"].as_u64().unwrap_or(0) > 0 { return None; }
    let (cwd, id) = (head["cwd"].as_str()?.to_string(), head["id"].as_str()?.to_string());
    let title = lines
        .filter(|l| l.contains("\"session/title\""))
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .filter_map(|v| v["data"]["title"].as_str().map(String::from))
        .last();
    Some((cwd, id, title))
}

// ---------- OpenClaw ----------

fn openclaw(home: &Path) -> Found {
    let state = env_dir("OPENCLAW_STATE_DIR", env_dir("OPENCLAW_HOME", home.join(".openclaw")));
    read_dir(&state.join("agents"))
        .into_iter()
        .filter_map(|agent| sqlite(&agent.join("agent/openclaw-agent.sqlite")))
        .flat_map(|c| query_openclaw(&c).unwrap_or_default())
        .collect()
}

/// Session id here is the session *key* (`agent:main:main`), which `openclaw tui --session` takes.
/// cwd lives only in the transcript header event.
fn query_openclaw(conn: &rusqlite::Connection) -> rusqlite::Result<Found> {
    let mut st = conn.prepare(
        "SELECT n.session_key, COALESCE(n.display_name, n.label), n.updated_at,
                (SELECT event_json FROM transcript_events t WHERE t.session_id = n.current_session_id ORDER BY seq LIMIT 1)
         FROM session_nodes n",
    )?;
    let rows = st.query_map([], |r| {
        let header: Option<String> = r.get(3)?;
        let cwd = header.and_then(|h| serde_json::from_str::<Value>(&h).ok()).and_then(|v| v["cwd"].as_str().map(String::from));
        let s = session("openclaw", r.get(0)?, r.get(1)?, r.get(2)?);
        Ok(cwd.map(|cwd| (cwd, s)))
    })?;
    rows.filter_map(Result::transpose).collect()
}

// ---------- Hermes Agent (Nous Research) ----------

fn hermes(home: &Path) -> Found {
    let default = if cfg!(windows) { dirs::data_local_dir().unwrap_or_default().join("hermes") } else { home.join(".hermes") };
    sqlite(&env_dir("HERMES_HOME", default).join("state.db"))
        .and_then(|c| query_hermes(&c).ok())
        .unwrap_or_default()
}

/// Timestamps are float seconds; last activity = newest message.
fn query_hermes(conn: &rusqlite::Connection) -> rusqlite::Result<Found> {
    let mut st = conn.prepare(
        "SELECT s.id, s.cwd, s.title,
                COALESCE((SELECT MAX(m.timestamp) FROM messages m WHERE m.session_id = s.id), s.started_at)
         FROM sessions s
         WHERE s.cwd IS NOT NULL AND s.parent_session_id IS NULL AND COALESCE(s.archived, 0) = 0",
    )?;
    let rows = st.query_map([], |r| {
        let secs: f64 = r.get(3)?;
        Ok((r.get(1)?, session("hermes", r.get(0)?, r.get(2)?, (secs * 1000.0) as i64)))
    })?;
    rows.collect()
}

/// Workspace folder from the `file:///…` URIs inside a protobuf blob.
/// The blob holds it twice (one copy has a trailing field byte); the shortest decoded one is clean.
fn file_uri_path(blob: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(blob);
    s.match_indices("file:///")
        .map(|(i, _)| {
            let rest = &s[i + 8..];
            let end = rest.find(|c: char| c < ' ' || c == '"' || c == '\u{FFFD}').unwrap_or(rest.len());
            percent_decode(&rest[..end])
        })
        .min_by_key(|p| p.len())
        .map(|p| if p.as_bytes().get(1) == Some(&b':') { p } else { format!("/{p}") })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        let hex = s.get(i + 1..i + 3).filter(|_| b[i] == b'%').and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(h) => { out.push(h); i += 3; }
            None => { out.push(b[i]); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------- helpers ----------

fn read_dir(p: &Path) -> Vec<PathBuf> {
    fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default()
}

/// README first prose line, else package.json / Cargo.toml description.
fn describe(dir: &Path) -> Option<String> {
    let readme = ["README.md", "readme.md", "Readme.md"].iter().find_map(|n| head(&dir.join(n)));
    if let Some(line) = readme.as_deref().and_then(|t| {
        let mut in_fence = false;
        t.lines().map(str::trim).find(|l| {
            if l.starts_with("```") { in_fence = !in_fence; return false; }
            !in_fence && !l.is_empty() && !["#", "!", "<", "[", "-", "=", "|", ">"].iter().any(|p| l.starts_with(p))
        })
    }) {
        return Some(clip(&plain(line)));
    }
    if let Some(d) = head(&dir.join("package.json"))
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["description"].as_str().filter(|s| !s.is_empty()).map(String::from))
    {
        return Some(clip(&d));
    }
    head(&dir.join("Cargo.toml"))?
        .lines()
        .find_map(|l| l.trim().strip_prefix("description")?.trim().strip_prefix('=').map(|v| clip(v.trim().trim_matches('"'))))
}

fn head(p: &Path) -> Option<String> {
    let mut buf = String::new();
    fs::File::open(p).ok()?.take(16 * 1024).read_to_string(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(src: &str) -> impl Iterator<Item = String> + '_ {
        src.lines().map(String::from)
    }

    #[test]
    fn claude_cwd_title_source() {
        let src = r#"{"type":"mode","sessionId":"a"}
{"type":"user","cwd":"C:/proj","entrypoint":"claude-vscode","message":{"content":"<command-name>x</command-name>"}}
{"type":"user","cwd":"C:/proj","message":{"content":[{"type":"text","text":"fix the bug"}]}}
{"type":"ai-title","aiTitle":"Bug hunt","sessionId":"a"}"#;
        assert_eq!(parse_claude(ls(src)), Some(("C:/proj".into(), Some("Bug hunt".into()), Some("VS Code".into()))));
        let cli = r#"{"type":"user","cwd":"/p","entrypoint":"cli","message":{"content":"hello  there"}}"#;
        assert_eq!(parse_claude(ls(cli)), Some(("/p".into(), Some("hello  there".into()), None)));
        assert_eq!(parse_claude(ls(r#"{"type":"summary","summary":"x"}"#)), None);
    }

    #[test]
    fn codex_meta_and_subagent_skip() {
        let src = r#"{"type":"session_meta","payload":{"id":"abc","cwd":"C:/w","source":"vscode"}}
{"type":"event_msg","payload":{"type":"user_message","message":"add tests"}}"#;
        assert_eq!(parse_codex(ls(src)), Some(("C:/w".into(), "abc".into(), Some("add tests".into()))));
        let sub = r#"{"type":"session_meta","payload":{"id":"s","cwd":"C:/w","source":{"subagent":{}}}}"#;
        assert_eq!(parse_codex(ls(sub)), None);
    }

    #[test]
    fn pi_head_and_first_message() {
        let src = r#"{"type":"session","version":3,"id":"019e-ab","cwd":"C:/W/x y"}
{"type":"model_change","id":"1"}
{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#;
        assert_eq!(parse_pi(ls(src)), Some(("C:/W/x y".into(), "019e-ab".into(), Some("hello".into()))));
        assert_eq!(parse_pi(ls(r#"{"type":"message"}"#)), None);
    }

    #[test]
    fn antigravity_workspace_from_blob() {
        let blob = b"\x0a\x10file:///c%3A/WORKSPACE/video%20edting/vz\xc4\x12file:///c:/WORKSPACE/video%20edting/v\x1a";
        assert_eq!(file_uri_path(blob).as_deref(), Some("c:/WORKSPACE/video edting/v"));
        assert_eq!(file_uri_path(b"\x0afile:///Users/me/app\x12").as_deref(), Some("/Users/me/app"));
        assert_eq!(file_uri_path(b"nothing"), None);
    }

    #[test]
    fn opencode_query() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE session(id, directory, title, time_updated, parent_id, time_archived);
             INSERT INTO session VALUES('ses_1','C:/a b','Main',5,NULL,NULL),
               ('ses_2','C:/a b','child',6,'ses_1',NULL),('ses_3','C:/a b','old',7,NULL,9);",
        )
        .unwrap();
        let r = query_opencode(&c).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].0.as_str(), r[0].1.id.as_str()), ("C:/a b", "ses_1"));
    }

    #[test]
    fn dsh_head_and_last_title() {
        let src = r#"{"type":"session","version":4,"id":"session-c7ab","cwd":"C:/W/app","delegationDepth":0}
{"type":"session/title","seq":15,"data":{"title":"u can make a videos","source":{"kind":"fallback"}}}
{"type":"session/title","seq":19,"data":{"title":"Video-making question","source":{"kind":"provider"}}}"#;
        assert_eq!(parse_dsh(ls(src)), Some(("C:/W/app".into(), "session-c7ab".into(), Some("Video-making question".into()))));
        let sub = r#"{"type":"session","id":"s","cwd":"C:/W","delegationDepth":1}"#;
        assert_eq!(parse_dsh(ls(sub)), None);
    }

    #[test]
    fn deepseek_metadata() {
        let v = serde_json::json!({"schema_version":1,"metadata":{"id":"8005-96e6","title":"Fix CI","workspace":"/w/app"},"messages":[]});
        assert_eq!(parse_deepseek(&v), Some(("/w/app".into(), "8005-96e6".into(), Some("Fix CI".into()))));
    }

    #[test]
    fn openclaw_and_hermes_queries() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"CREATE TABLE session_nodes(session_key, current_session_id, label, display_name, updated_at);
               CREATE TABLE transcript_events(session_id, seq, event_json, created_at);
               INSERT INTO session_nodes VALUES('agent:main:main','s1','main',NULL,1700000000000),('agent:main:x','s2',NULL,NULL,1);
               INSERT INTO transcript_events VALUES('s1',0,'{"type":"session","id":"s1","cwd":"/home/me"}',0);
               CREATE TABLE sessions(id, cwd, title, started_at, parent_session_id, archived);
               CREATE TABLE messages(session_id, timestamp);
               INSERT INTO sessions VALUES('20260101_1_ab','/p','Plan',10.0,NULL,0),('child','/p',NULL,11.0,'20260101_1_ab',0);
               INSERT INTO messages VALUES('20260101_1_ab',12.5);"#,
        )
        .unwrap();
        let oc = query_openclaw(&c).unwrap();
        assert_eq!(oc.len(), 1, "session without transcript cwd is skipped");
        assert_eq!((oc[0].0.as_str(), oc[0].1.id.as_str(), oc[0].1.title.as_str()), ("/home/me", "agent:main:main", "main"));
        let h = query_hermes(&c).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].1.id.as_str(), h[0].1.updated), ("20260101_1_ab", 12500));
    }

    #[test]
    fn readme_line_to_plain() {
        assert_eq!(plain("A [Next.js](https://x) app with [`cna`](https://y)."), "A Next.js app with cna.");
        assert_eq!(plain("**Bold** [x]"), "Bold [x]");
    }

    #[test]
    fn paths_merge() {
        if cfg!(windows) {
            assert_eq!(path_key(r"C:\WORKSPACE\X\"), path_key("c:/workspace/x"));
        }
    }
}

/// Smoke check against this machine's real data: `cargo test real_scan -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_scan() {
    let t = std::time::Instant::now();
    let ps = scan_all();
    for p in ps.iter().take(15) {
        let h: Vec<_> = p.sessions.iter().map(|s| s.harness).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        println!("{:<45} {:>3} {:?} | {}", p.path, p.sessions.len(), h, p.description);
    }
    let mut per: std::collections::BTreeMap<&str, usize> = Default::default();
    ps.iter().flat_map(|p| &p.sessions).for_each(|s| *per.entry(s.harness).or_default() += 1);
    println!("{} projects in {:?}, sessions per harness: {per:?}", ps.len(), t.elapsed());
}
