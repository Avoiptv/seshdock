use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Serialize)]
pub struct App {
    pub id: String,
    pub name: String,
    pub kind: &'static str, // "terminal" | "ide" | "claude-ext" | "obsidian" | "dsh" | "codex-app"
}

fn valid_id(id: &str) -> Result<&str, String> {
    // ':' for OpenClaw keys like `agent:main:main`; none of these chars are special to PowerShell or sh
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_:.".contains(c));
    ok.then_some(id).ok_or_else(|| "invalid session id".into())
}

/// Shell command that resumes a session. Built here, never taken from the UI.
fn resume_cmd(harness: &str, id: &str) -> Result<String, String> {
    let id = valid_id(id)?;
    Ok(match harness {
        "claude" => format!("claude --resume {id}"),
        "codex" => format!("codex resume {id}"),
        "opencode" => format!("opencode --session {id}"),
        "pi" => format!("pi --session {id}"),
        "antigravity" => format!("agy --conversation {id}"),
        "deepseek" => format!("codewhale --resume {id}"), // DeepSeek TUI was renamed Codewhale
        "hermes" => format!("hermes --resume {id}"),
        "openclaw" => format!("openclaw tui --session {id}"),
        _ => return Err(format!("unknown harness {harness}")),
    })
}

pub fn open_in(app: &str, dir: &str, harness: Option<&str>, session: Option<&str>) -> Result<(), String> {
    if !Path::new(dir).is_dir() {
        return Err(format!("folder not found: {dir}"));
    }
    let app = detect().into_iter().find(|a| a.id == app).ok_or("app not available")?;
    match app.kind {
        "terminal" => {
            let cmd = match (harness, session) {
                (Some("dsh"), _) => None, // DeepSeek Harness has no CLI: terminal just opens in the folder
                (Some(h), Some(s)) => Some(resume_cmd(h, s)?),
                _ => None,
            };
            open_terminal(&app.id, dir, cmd.as_deref()).map_err(|e| e.to_string())
        }
        "claude-ext" => {
            let id = valid_id(session.filter(|_| harness == Some("claude")).ok_or("needs a Claude Code session")?)?;
            let (_, ide) = app.id.split_once(':').unwrap_or_default();
            let scheme = CLAUDE_EXT.iter().find(|e| e.0 == ide).map_or("vscode", |e| e.1);
            open_ide(ide, dir).map_err(|e| e.to_string())?;
            // ponytail: fixed delay for the editor window to come up; poll if 2s proves flaky on slow machines
            let url = format!("{scheme}://anthropic.claude-code/open?session={id}");
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let _ = tauri_plugin_opener::open_url(url, None::<&str>);
            });
            Ok(())
        }
        // ponytail: dsh://open only focuses the app (no per-session deep link as of v1); the session is in its sidebar
        "dsh" => tauri_plugin_opener::open_url("dsh://open", None::<&str>).map_err(|e| e.to_string()),
        "codex-app" => {
            let id = valid_id(session.filter(|_| harness == Some("codex")).ok_or("needs a Codex session")?)?;
            tauri_plugin_opener::open_url(format!("codex://threads/{id}"), None::<&str>).map_err(|e| e.to_string())
        }
        "obsidian" => tauri_plugin_opener::open_url(format!("obsidian://open?path={}", encode(dir)), None::<&str>)
            .map_err(|e| e.to_string()),
        _ => open_ide(&app.id, dir).map_err(|e| e.to_string()),
    }
}

pub fn detect() -> Vec<App> {
    let home = dirs::home_dir().unwrap_or_default();
    let ides = detect_ides();
    let mut apps: Vec<App> = detect_terminals()
        .into_iter()
        .map(|(id, name)| App { id: id.into(), name: name.into(), kind: "terminal" })
        .collect();
    // Claude Code extension installed in an editor → can resume inside its panel
    for (ide, _, ext_dir, label) in CLAUDE_EXT {
        let has_ext = std::fs::read_dir(home.join(ext_dir).join("extensions"))
            .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("anthropic.claude-code")))
            .unwrap_or(false);
        if has_ext && ides.iter().any(|(id, _)| id == &ide) {
            apps.push(App { id: format!("claude-ext:{ide}"), name: format!("Claude Code in {label}"), kind: "claude-ext" });
        }
    }
    apps.extend(ides.into_iter().map(|(id, name)| App { id: id.into(), name: name.into(), kind: "ide" }));
    if codex_app_installed() {
        apps.push(App { id: "codex-app".into(), name: "Codex app".into(), kind: "codex-app" });
    }
    if dsh_installed() {
        apps.push(App { id: "dsh".into(), name: "DeepSeek Harness".into(), kind: "dsh" });
    }
    if obsidian_installed() {
        apps.push(App { id: "obsidian".into(), name: "Obsidian".into(), kind: "obsidian" });
    }
    apps
}

/// CLI each harness resumes with (see `resume_cmd`).
const CLIS: [(&str, &str); 8] = [
    ("claude", "claude"),
    ("codex", "codex"),
    ("opencode", "opencode"),
    ("pi", "pi"),
    ("antigravity", "agy"),
    ("deepseek", "codewhale"),
    ("hermes", "hermes"),
    ("openclaw", "openclaw"),
];

/// Harnesses whose CLI isn't on PATH, so "resume in terminal" would fail.
/// ponytail: macOS GUI apps get a minimal PATH, so there we assume installed and let the terminal report it.
pub fn missing_clis() -> Vec<&'static str> {
    if cfg!(windows) { CLIS.iter().filter(|(_, bin)| on_path(bin).is_none()).map(|(h, _)| *h).collect() } else { Vec::new() }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------- Windows ----------

/// (cli name, display name, exe under %LOCALAPPDATA%\Programs as fallback)
#[cfg(windows)]
const IDES: [(&str, &str, &str); 6] = [
    ("code", "VS Code", r"Microsoft VS Code\Code.exe"),
    ("cursor", "Cursor", r"cursor\Cursor.exe"),
    ("windsurf", "Windsurf", r"Windsurf\Windsurf.exe"),
    ("zed", "Zed", r"Zed\zed.exe"),
    ("antigravity", "Antigravity", r"Antigravity\Antigravity.exe"),
    ("antigravity-ide", "Antigravity IDE", r"Antigravity IDE\Antigravity IDE.exe"),
];

/// (ide id, URI scheme, config dir under home, label)
#[cfg(windows)]
const CLAUDE_EXT: [(&str, &str, &str, &str); 2] = [("code", "vscode", ".vscode", "VS Code"), ("cursor", "cursor", ".cursor", "Cursor")];

fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat", "ps1"] } else { &[""] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| {
        exts.iter().map(|e| if e.is_empty() { d.join(name) } else { d.join(format!("{name}.{e}")) }).find(|p| p.is_file())
    })
}

#[cfg(windows)]
fn ide_path(id: &str) -> Option<PathBuf> {
    let (bin, _, exe) = IDES.iter().find(|i| i.0 == id)?;
    on_path(bin).or_else(|| Some(dirs::data_local_dir()?.join("Programs").join(exe)).filter(|p| p.is_file()))
}

#[cfg(windows)]
fn detect_terminals() -> Vec<(&'static str, &'static str)> {
    vec![if on_path("wt").is_some() { ("wt", "Windows Terminal") } else { ("powershell", "PowerShell") }]
}

#[cfg(windows)]
fn detect_ides() -> Vec<(&'static str, &'static str)> {
    IDES.iter().filter(|i| ide_path(i.0).is_some()).map(|i| (i.0, i.1)).collect()
}

#[cfg(windows)]
fn codex_app_installed() -> bool {
    // Store app: its per-user data folder exists once installed
    let pkgs = dirs::data_local_dir().unwrap_or_default().join("Packages");
    std::fs::read_dir(pkgs).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("OpenAI.Codex_"))).unwrap_or(false)
}

#[cfg(windows)]
fn dsh_installed() -> bool {
    dirs::data_local_dir().unwrap_or_default().join(r"Programs\DeepSeek Harness\DeepSeek Harness.exe").is_file()
}

#[cfg(windows)]
fn obsidian_installed() -> bool {
    let local = dirs::data_local_dir().unwrap_or_default().join(r"Programs\Obsidian\Obsidian.exe");
    local.is_file() || Path::new(r"C:\Program Files\Obsidian\Obsidian.exe").is_file()
}

#[cfg(windows)]
fn open_terminal(id: &str, dir: &str, cmd: Option<&str>) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x10;
    // Command words go as separate args; PowerShell joins everything after -Command.
    let ps_args: Vec<&str> = match cmd {
        Some(c) => ["-NoExit", "-Command"].into_iter().chain(c.split(' ')).collect(),
        None => vec!["-NoExit"],
    };
    if id == "wt" {
        Command::new("wt").args(["-d", dir, "powershell"]).args(&ps_args).spawn()?;
    } else {
        Command::new("powershell").args(&ps_args).current_dir(dir).creation_flags(CREATE_NEW_CONSOLE).spawn()?;
    }
    Ok(())
}

#[cfg(windows)]
fn open_ide(id: &str, dir: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let path = ide_path(id).ok_or_else(|| std::io::Error::other(format!("{id} not found")))?;
    Command::new(path).arg(dir).creation_flags(CREATE_NO_WINDOW).spawn()?;
    Ok(())
}

// ---------- macOS ----------

#[cfg(target_os = "macos")]
const IDES: [(&str, &str); 6] = [
    ("Visual Studio Code", "VS Code"),
    ("Cursor", "Cursor"),
    ("Windsurf", "Windsurf"),
    ("Zed", "Zed"),
    ("Antigravity", "Antigravity"),
    ("Antigravity IDE", "Antigravity IDE"),
];

#[cfg(target_os = "macos")]
const CLAUDE_EXT: [(&str, &str, &str, &str); 2] =
    [("Visual Studio Code", "vscode", ".vscode", "VS Code"), ("Cursor", "cursor", ".cursor", "Cursor")];

#[cfg(target_os = "macos")]
fn app_installed(name: &str) -> bool {
    let home = dirs::home_dir().unwrap_or_default();
    [PathBuf::from("/Applications"), home.join("Applications")].iter().any(|d| d.join(format!("{name}.app")).exists())
}

#[cfg(target_os = "macos")]
fn detect_terminals() -> Vec<(&'static str, &'static str)> {
    let mut t = vec![("Terminal", "Terminal")];
    if app_installed("iTerm") {
        t.push(("iTerm", "iTerm"));
    }
    t
}

#[cfg(target_os = "macos")]
fn detect_ides() -> Vec<(&'static str, &'static str)> {
    IDES.iter().filter(|i| app_installed(i.0)).copied().collect()
}

#[cfg(target_os = "macos")]
fn codex_app_installed() -> bool {
    app_installed("Codex")
}

#[cfg(target_os = "macos")]
fn dsh_installed() -> bool {
    app_installed("DeepSeek Harness")
}

#[cfg(target_os = "macos")]
fn obsidian_installed() -> bool {
    app_installed("Obsidian")
}

#[cfg(target_os = "macos")]
fn open_terminal(id: &str, dir: &str, cmd: Option<&str>) -> std::io::Result<()> {
    // Shell-quote the dir, then escape the whole line for an AppleScript string.
    let shell = format!("cd '{}'{}", dir.replace('\'', r"'\''"), cmd.map(|c| format!(" && {c}")).unwrap_or_default());
    let line = shell.replace('\\', r"\\").replace('"', "\\\"");
    let script = if id == "iTerm" {
        format!("tell application \"iTerm\"\nactivate\nset w to (create window with default profile)\ntell current session of w to write text \"{line}\"\nend tell")
    } else {
        format!("tell application \"Terminal\"\nactivate\ndo script \"{line}\"\nend tell")
    };
    Command::new("osascript").args(["-e", &script]).spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_ide(id: &str, dir: &str) -> std::io::Result<()> {
    Command::new("open").args(["-a", id, dir]).spawn()?;
    Ok(())
}

// ---------- other platforms: not supported yet ----------

#[cfg(not(any(windows, target_os = "macos")))]
const CLAUDE_EXT: [(&str, &str, &str, &str); 0] = [];
#[cfg(not(any(windows, target_os = "macos")))]
fn detect_terminals() -> Vec<(&'static str, &'static str)> {
    Vec::new()
}
#[cfg(not(any(windows, target_os = "macos")))]
fn detect_ides() -> Vec<(&'static str, &'static str)> {
    Vec::new()
}
#[cfg(not(any(windows, target_os = "macos")))]
fn codex_app_installed() -> bool {
    false
}
#[cfg(not(any(windows, target_os = "macos")))]
fn dsh_installed() -> bool {
    false
}
#[cfg(not(any(windows, target_os = "macos")))]
fn obsidian_installed() -> bool {
    false
}
#[cfg(not(any(windows, target_os = "macos")))]
fn open_terminal(_: &str, _: &str, _: Option<&str>) -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported platform"))
}
#[cfg(not(any(windows, target_os = "macos")))]
fn open_ide(_: &str, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported platform"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_rejects_injection() {
        assert_eq!(resume_cmd("codex", "01a1-b_c").unwrap(), "codex resume 01a1-b_c");
        assert_eq!(resume_cmd("openclaw", "agent:main:main").unwrap(), "openclaw tui --session agent:main:main");
        assert!(resume_cmd("claude", "x; rm -rf ~").is_err());
        assert!(resume_cmd("claude", "").is_err());
        assert!(resume_cmd("vim", "abc").is_err());
    }

    #[test]
    fn encodes_paths() {
        assert_eq!(encode("C:\\a b/c"), "C%3A%5Ca%20b%2Fc");
    }
}
