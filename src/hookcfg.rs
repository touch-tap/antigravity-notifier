//! Installs, removes and inspects antigravity-notifier's entries in Antigravity's global hooks file
//! (`~/.gemini/config/hooks.json`, shared by the `agy` CLI, the desktop app and the IDE).
//! Everything else in that file is left exactly as it was.
//!
//! The file's top-level keys are named hook groups; ours is `antigravity-notifier`. Inside a
//! group, SessionStart, PreInvocation, PostInvocation and Stop are flat lists of handlers
//! (`{type, command, timeout}`), while PreToolUse and PostToolUse are lists of `{matcher, hooks}`.
//! Measured on agy 1.5.18 (26 Nov 2026): the global file loads with no trust step, and the
//! payload carries its own event name, but the event is also passed as an argument so a payload
//! that can't be read still gets the right answer (see hook.rs on why that matters).

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;

/// The name of our group in hooks.json
pub const GROUP: &str = "antigravity-notifier";

/// The longest a PreToolUse hook holds a tool call for the phone, whatever the hold ceiling
/// setting says. Holds of 600 s were measured to complete on agy 1.5.18; longer ones were not
/// tried.
pub const MAX_TOOL_HOLD: u64 = 600;
/// PreToolUse's timeout: the longest hold plus a margin. 900 was measured to be accepted.
/// Antigravity blocks the tool call when a hook times out, so hook.rs answers on its own well
/// before this (`hook::PRE_TOOL_DEADLINE`).
pub const PRE_TOOL_TIMEOUT: u64 = 900;

/// Event name, whether its entries sit under a matcher, timeout in seconds. Stop covers the
/// longest `stop_hold` (600 s) plus a margin.
pub const EVENTS: &[(&str, bool, u64)] = &[
    ("SessionStart", false, 30),
    ("PreInvocation", false, 10),
    ("PreToolUse", true, PRE_TOOL_TIMEOUT),
    ("PostToolUse", true, 10),
    ("Stop", false, 700),
];

/// Every tool
const MATCHER: &str = "*";

pub struct InstallReport {
    pub hooks_path: PathBuf,
    pub exe: PathBuf,
    /// False when hooks.json already held exactly this group, so it was not touched
    pub changed: bool,
}

pub struct Status {
    pub hooks_path: PathBuf,
    pub installed: Vec<String>,
    pub missing: Vec<String>,
    pub stale_exe: bool,
}

/// The path the hooks call. It has to survive upgrades: a Homebrew Cellar path is versioned, so
/// it maps to the prefix's bin link, and a binary run from npx's cache (the npm wrapper sets
/// `ANTIGRAVITY_NOTIFIER_EPHEMERAL=1`) is replaced by the copy `install_self` puts in the user's bin directory.
pub fn exe_path() -> Result<PathBuf> {
    if ephemeral() {
        return Ok(user_bin_dir().join(exe_name()));
    }
    let exe = undeleted(std::env::current_exe().context("finding the antigravity-notifier executable")?);
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(homebrew_bin(&exe).unwrap_or(exe))
}

/// Linux reports a binary that has been replaced while running (as `antigravity-notifier upgrade` does to
/// itself) as `/path/antigravity-notifier (deleted)`. The new binary is at the original path, so that is
/// the one to use.
fn undeleted(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) if !exe.exists() => PathBuf::from(path),
        _ => exe,
    }
}

fn ephemeral() -> bool {
    std::env::var("ANTIGRAVITY_NOTIFIER_EPHEMERAL")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "antigravity-notifier.exe"
    } else {
        "antigravity-notifier"
    }
}

/// Where the install scripts and `install_self` put the binary.
pub fn user_bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ANTIGRAVITY_NOTIFIER_INSTALL_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local).join("Programs").join("antigravity-notifier");
            }
        }
    }
    config::home_dir().join(".local").join("bin")
}

/// `/opt/homebrew/Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier` → `/opt/homebrew/bin/antigravity-notifier`, when that link exists.
fn homebrew_bin(exe: &Path) -> Option<PathBuf> {
    let s = exe.to_str()?;
    let (prefix, rest) = s.split_once("/Cellar/antigravity-notifier/")?;
    let name = rest.rsplit('/').next()?;
    let link = Path::new(prefix).join("bin").join(name);
    link.exists().then_some(link)
}

/// Copies a binary running from npx's cache to the user's bin directory, so the hooks keep
/// working after the cache is cleared. Returns the copy's path when one was made.
pub fn install_self() -> Result<Option<PathBuf>> {
    if !ephemeral() {
        return Ok(None);
    }
    let from = std::env::current_exe().context("finding the antigravity-notifier executable")?;
    let dir = user_bin_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let to = dir.join(exe_name());
    // Already this very build: nothing to do (so a repeated setup stays quiet)
    if let (Ok(a), Ok(b)) = (fs::read(&from), fs::read(&to))
        && a == b
    {
        return Ok(None);
    }
    let tmp = dir.join(format!("{}.new", exe_name()));
    fs::copy(&from, &tmp)
        .with_context(|| format!("copying antigravity-notifier to {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&tmp, &to).with_context(|| format!("installing {}", to.display()))?;
    Ok(Some(to))
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The command Antigravity runs for `event`: the binary's absolute path, `hook` and the event's
/// name. One string through the user's shell on macOS and Linux; the Windows form is quoted for
/// cmd/PowerShell.
pub fn hook_command(exe: &Path, event: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\" hook {event}", exe.to_string_lossy())
    } else {
        format!("{} hook {event}", shell_quote(exe))
    }
}

fn hook_entry(exe: &Path, event: &str, timeout: u64) -> Value {
    json!({ "type": "command", "command": hook_command(exe, event), "timeout": timeout })
}

fn command_of(entry: &Value) -> &str {
    entry.get("command").and_then(|c| c.as_str()).unwrap_or("")
}

/// Whether a handler runs antigravity-notifier's hook: `<path>/antigravity-notifier hook [Event]`.
pub fn is_ours(entry: &Value) -> bool {
    let command = command_of(entry).trim();
    let Some(at) = command.rfind(" hook") else {
        return false;
    };
    let rest = command[at + " hook".len()..].trim();
    if !(rest.is_empty() || EVENTS.iter().any(|(event, _, _)| *event == rest)) {
        return false;
    }
    let exe = command[..at].trim().trim_matches('\'').trim_matches('"');
    // Both separators: a Windows path in a file read on another platform still counts
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    base == "antigravity-notifier" || base == "antigravity-notifier.exe"
}

// ============================================
// hooks.json
// ============================================

/// What reading hooks.json found: its top-level object, and whether a file was there at all.
/// A missing or empty file is an empty object. A file that is not a JSON object is an error:
/// writing over it would throw away hooks the user can still repair by hand.
fn load_hooks(path: &Path) -> Result<(serde_json::Map<String, Value>, bool)> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((serde_json::Map::new(), false));
        }
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let text = String::from_utf8(bytes)
        .with_context(|| format!("{} is not UTF-8 text, so it was left alone", path.display()))?;
    // Editors on Windows put a byte order mark first
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok((serde_json::Map::new(), true));
    }
    let root: Value = serde_json::from_str(text).with_context(|| {
        format!(
            "{} is not valid JSON, so it was left alone: fix or remove it, then run `antigravity-notifier setup`",
            path.display()
        )
    })?;
    match root {
        Value::Object(map) => Ok((map, true)),
        _ => anyhow::bail!(
            "{} is not a JSON object, so it was left alone: fix or remove it, then run `antigravity-notifier setup`",
            path.display()
        ),
    }
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.antigravity-notifier-backup")
}

/// Writes the file in one step (Antigravity may read it at any moment), keeping a copy of what
/// was there before our first change.
fn save_hooks(path: &Path, root: &serde_json::Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    if path.exists() {
        let backup = backup_path(path);
        if !backup.exists() {
            let _ = fs::copy(path, &backup);
        }
    }
    let text = serde_json::to_string_pretty(root)?;
    config::write_atomic(path, format!("{text}\n").as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Our group, as it should be for a binary at `exe`.
fn our_group(exe: &Path) -> Value {
    let mut group = serde_json::Map::new();
    for (event, matched, timeout) in EVENTS {
        let entry = hook_entry(exe, event, *timeout);
        let list = if *matched {
            json!([{ "matcher": MATCHER, "hooks": [entry] }])
        } else {
            json!([entry])
        };
        group.insert((*event).to_string(), list);
    }
    Value::Object(group)
}

/// The handlers of `event` in a group, whichever of the two shapes they are in.
fn handlers<'a>(group: &'a Value, event: &str) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for item in group
        .get(event)
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
    {
        match item.get("hooks").and_then(|h| h.as_array()) {
            Some(nested) => out.extend(nested.iter()),
            None => out.push(item),
        }
    }
    out
}

fn install_at(hooks_path: &Path, exe: &Path) -> Result<bool> {
    let (mut root, existed) = load_hooks(hooks_path)?;
    let group = our_group(exe);
    // Idempotent: a second run finds its own group and writes nothing
    if existed && root.get(GROUP) == Some(&group) {
        return Ok(false);
    }
    root.insert(GROUP.to_string(), group);
    save_hooks(hooks_path, &root)?;
    Ok(true)
}

pub fn install() -> Result<InstallReport> {
    let hooks_path = config::hooks_path();
    let exe = exe_path()?;
    let changed = install_at(&hooks_path, &exe)?;
    Ok(InstallReport {
        hooks_path,
        exe,
        changed,
    })
}

fn uninstall_at(hooks_path: &Path) -> Result<()> {
    let (mut root, existed) = load_hooks(hooks_path)?;
    if !existed || root.remove(GROUP).is_none() {
        return Ok(());
    }
    // No backup means there was no file before ours, so an empty one goes too
    if root.is_empty() && !backup_path(hooks_path).exists() {
        let _ = fs::remove_file(hooks_path);
        return Ok(());
    }
    save_hooks(hooks_path, &root)
}

pub fn uninstall() -> Result<PathBuf> {
    let hooks_path = config::hooks_path();
    uninstall_at(&hooks_path)?;
    Ok(hooks_path)
}

fn status_at(hooks_path: &Path, exe: &str) -> Result<(Vec<String>, Vec<String>, bool)> {
    let (root, _) = load_hooks(hooks_path)?;
    let group = root.get(GROUP).cloned().unwrap_or(Value::Null);
    let mut installed = Vec::new();
    let mut missing = Vec::new();
    let mut stale_exe = false;
    for (event, _, _) in EVENTS {
        let ours: Vec<&Value> = handlers(&group, event)
            .into_iter()
            .filter(|entry| is_ours(entry))
            .collect();
        if ours.is_empty() {
            missing.push(event.to_string());
            continue;
        }
        installed.push(event.to_string());
        if !exe.is_empty() && ours.iter().any(|entry| !command_of(entry).contains(exe)) {
            stale_exe = true;
        }
    }
    Ok((installed, missing, stale_exe))
}

pub fn status() -> Result<Status> {
    let hooks_path = config::hooks_path();
    let exe = exe_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let (installed, missing, stale_exe) = status_at(&hooks_path, &exe)?;
    Ok(Status {
        hooks_path,
        installed,
        missing,
        stale_exe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("agn-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    const EXE: &str = "/usr/local/bin/antigravity-notifier";

    #[test]
    #[cfg(unix)] // Homebrew paths are Unix paths
    fn homebrew_cellar_maps_to_the_bin_link() {
        let root = scratch("brew");
        let cellar = root.join("Cellar/antigravity-notifier/0.1.0/bin");
        fs::create_dir_all(&cellar).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(cellar.join("antigravity-notifier"), "").unwrap();
        fs::write(root.join("bin/antigravity-notifier"), "").unwrap();
        assert_eq!(
            homebrew_bin(&cellar.join("antigravity-notifier")),
            Some(root.join("bin/antigravity-notifier"))
        );
        assert_eq!(
            homebrew_bin(&root.join("elsewhere/antigravity-notifier")),
            None
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recognises_our_entries() {
        for command in [
            "/opt/homebrew/bin/antigravity-notifier hook PreToolUse",
            "'/Users/me/my bin/antigravity-notifier' hook Stop",
            "\"C:\\Users\\me\\antigravity-notifier.exe\" hook SessionStart",
            "/opt/homebrew/bin/antigravity-notifier hook",
        ] {
            assert!(is_ours(&json!({ "type": "command", "command": command })), "{command}");
        }
        for command in [
            "ccn hook",
            "codex-notifier hook",
            "antigravity-notifier doctor",
            "/x/antigravity-notifier hook && rm -rf /",
            "/x/hook.sh PreToolUse",
        ] {
            assert!(!is_ours(&json!({ "type": "command", "command": command })), "{command}");
        }
    }

    #[test]
    fn the_group_has_the_shape_antigravity_reads() {
        let group = our_group(Path::new(EXE));
        // Flat lists of handlers
        assert_eq!(
            group["Stop"],
            json!([{ "type": "command", "command": format!("{} hook Stop", hook_exe()), "timeout": 700 }])
        );
        assert!(group["SessionStart"][0].get("matcher").is_none());
        assert!(group["PreInvocation"][0]["command"].as_str().unwrap().ends_with(" hook PreInvocation"));
        // Matcher groups for the tool events
        assert_eq!(group["PreToolUse"][0]["matcher"], "*");
        let pre = &group["PreToolUse"][0]["hooks"][0];
        assert_eq!(pre["type"], "command");
        assert!(pre["command"].as_str().unwrap().ends_with(" hook PreToolUse"));
        assert_eq!(group["PostToolUse"][0]["matcher"], "*");
        // The hold fits inside the timeout with room to answer
        assert!(pre["timeout"].as_u64().unwrap() >= MAX_TOOL_HOLD + 120);
        assert_eq!(group.as_object().unwrap().len(), EVENTS.len());
    }

    fn hook_exe() -> String {
        if cfg!(windows) {
            format!("\"{EXE}\"")
        } else {
            EXE.to_string()
        }
    }

    #[test]
    fn install_keeps_other_groups_and_uninstall_restores_the_file() {
        let root = scratch("hooks");
        let path = root.join("config/hooks.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = r#"{
  "zeta": { "Stop": [ { "type": "command", "command": "echo hi", "timeout": 5 } ] },
  "alpha": { "PreToolUse": [ { "matcher": "run_command", "hooks": [ { "type": "command", "command": "/x/policy.sh" } ] } ], "note": 7 }
}"#;
        fs::write(&path, original).unwrap();

        assert!(install_at(&path, Path::new(EXE)).unwrap());
        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let before: Value = serde_json::from_str(original).unwrap();
        assert_eq!(saved["zeta"], before["zeta"]);
        assert_eq!(saved["alpha"], before["alpha"]);
        assert_eq!(saved[GROUP], our_group(Path::new(EXE)));
        // The user's groups stay first and in their order
        let keys: Vec<&String> = saved.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["zeta", "alpha", GROUP]);
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), original);

        // A second install changes nothing, byte for byte
        let text = fs::read_to_string(&path).unwrap();
        assert!(!install_at(&path, Path::new(EXE)).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);

        let (installed, missing, stale) = status_at(&path, EXE).unwrap();
        assert_eq!(installed.len(), EVENTS.len());
        assert!(missing.is_empty() && !stale);
        // A binary somewhere else: the hooks are stale, and install repairs them
        let (_, _, stale) = status_at(&path, "/opt/other/antigravity-notifier").unwrap();
        assert!(stale);
        assert!(install_at(&path, Path::new("/opt/other/antigravity-notifier")).unwrap());
        let (_, _, stale) = status_at(&path, "/opt/other/antigravity-notifier").unwrap();
        assert!(!stale);

        uninstall_at(&path).unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, before);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_missing_file_is_created_and_removed_again() {
        let root = scratch("hooks-new");
        let path = root.join("config/hooks.json");
        let (installed, missing, _) = status_at(&path, EXE).unwrap();
        assert!(installed.is_empty());
        assert_eq!(missing.len(), EVENTS.len());
        assert!(install_at(&path, Path::new(EXE)).unwrap());
        assert!(path.exists() && !backup_path(&path).exists());
        uninstall_at(&path).unwrap();
        assert!(!path.exists(), "a file that held only our group goes");
        // Uninstalling twice, or with nothing there, is fine
        uninstall_at(&path).unwrap();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_empty_file_that_was_the_users_stays() {
        let root = scratch("hooks-empty");
        let path = root.join("hooks.json");
        fs::write(&path, "{}\n").unwrap();
        assert!(install_at(&path, Path::new(EXE)).unwrap());
        uninstall_at(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), "{}");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_byte_order_mark_is_tolerated() {
        let root = scratch("hooks-bom");
        let path = root.join("hooks.json");
        fs::write(&path, "\u{feff}{ \"mine\": { \"Stop\": [] } }").unwrap();
        assert!(install_at(&path, Path::new(EXE)).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.starts_with('\u{feff}'));
        let saved: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(saved["mine"], json!({ "Stop": [] }));
        assert!(saved[GROUP].is_object());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn invalid_json_is_refused_and_left_alone() {
        let root = scratch("hooks-bad");
        let path = root.join("hooks.json");
        for broken in ["{ \"mine\": { \"Stop\": [ ", "[1, 2]", "\"text\""] {
            fs::write(&path, broken).unwrap();
            let err = install_at(&path, Path::new(EXE)).unwrap_err();
            assert!(format!("{err:#}").contains("left alone"), "{err:#}");
            assert!(uninstall_at(&path).is_err());
            assert!(status_at(&path, EXE).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), broken);
            assert!(!backup_path(&path).exists());
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_group_edited_by_hand_reports_what_is_missing() {
        let root = scratch("hooks-partial");
        let path = root.join("hooks.json");
        let mut group = our_group(Path::new(EXE));
        group.as_object_mut().unwrap().remove("Stop");
        group["PreToolUse"] = json!("nonsense");
        fs::write(&path, json!({ GROUP: group }).to_string()).unwrap();
        let (installed, missing, _) = status_at(&path, EXE).unwrap();
        assert_eq!(missing, ["PreToolUse", "Stop"]);
        assert_eq!(installed.len(), EVENTS.len() - 2);
        fs::remove_dir_all(root).ok();
    }
}
