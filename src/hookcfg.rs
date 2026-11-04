//! Installs, removes and inspects codexnotifier's entries in Codex's hooks file
//! (`$CODEX_HOME/hooks.json`), and records each entry's trust hash in `config.toml` so Codex runs
//! them straight away instead of waiting for a visit to `/hooks`. Everything else in both files is
//! left exactly as it was.
//!
//! Codex only runs a hook whose `[hooks.state."<key>"]` entry carries a `trusted_hash` equal to
//! the hash it computes from the hook's normalised definition (codex-rs/hooks/src/engine/
//! discovery.rs, `hook_hash`): the event name, the matcher and the handler with its timeout, as
//! TOML, as JSON with sorted keys, SHA-256. The key is `<hooks.json path>:<event>:<group>:<handler>`.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table};

use crate::config;

/// Codex event name, its key label, matcher, timeout in seconds. Blocking events get a timeout
/// above the largest hold ceiling. SessionEnd is clamped to 3 s by Codex, so it gets that.
pub const EVENTS: &[(&str, &str, Option<&str>, u64)] = &[
    ("SessionStart", "session_start", None, 30),
    ("UserPromptSubmit", "user_prompt_submit", None, 10),
    ("PreToolUse", "pre_tool_use", None, 3660),
    ("PermissionRequest", "permission_request", None, 3660),
    ("Stop", "stop", None, 700),
    ("SessionEnd", "session_end", None, 3),
];

pub struct InstallReport {
    pub hooks_path: PathBuf,
    pub config_path: PathBuf,
    pub exe: PathBuf,
    /// False when hooks.json and the trust entries were already exactly these, so neither file
    /// was touched
    pub changed: bool,
}

pub struct Status {
    pub hooks_path: PathBuf,
    pub installed: Vec<String>,
    pub missing: Vec<String>,
    /// Installed entries whose trust hash in config.toml is missing or different, so Codex skips them
    pub untrusted: Vec<String>,
    pub stale_exe: bool,
}

/// The path the hooks call. It has to survive upgrades: a Homebrew Cellar path is versioned, so
/// it maps to the prefix's bin link, and a binary run from npx's cache (the npm wrapper sets
/// `CODEXNOTIFIER_EPHEMERAL=1`) is replaced by the copy `install_self` puts in the user's bin directory.
pub fn exe_path() -> Result<PathBuf> {
    if ephemeral() {
        return Ok(user_bin_dir().join(exe_name()));
    }
    let exe = undeleted(std::env::current_exe().context("finding the codexnotifier executable")?);
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(homebrew_bin(&exe).unwrap_or(exe))
}

/// Linux reports a binary that has been replaced while running (as `codexnotifier upgrade` does to
/// itself) as `/path/codexnotifier (deleted)`. The new binary is at the original path, so that is
/// the one to use.
fn undeleted(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) if !exe.exists() => PathBuf::from(path),
        _ => exe,
    }
}

fn ephemeral() -> bool {
    std::env::var("CODEXNOTIFIER_EPHEMERAL")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "codexnotifier.exe"
    } else {
        "codexnotifier"
    }
}

/// Where the install scripts and `install_self` put the binary.
pub fn user_bin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEXNOTIFIER_INSTALL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.is_empty() {
                return PathBuf::from(local)
                    .join("Programs")
                    .join("codexnotifier");
            }
        }
    }
    config::home_dir().join(".local").join("bin")
}

/// `/opt/homebrew/Cellar/codexnotifier/0.1.0/bin/codexnotifier` → `/opt/homebrew/bin/codexnotifier`, when that link exists.
fn homebrew_bin(exe: &Path) -> Option<PathBuf> {
    let s = exe.to_str()?;
    let (prefix, rest) = s.split_once("/Cellar/codexnotifier/")?;
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
    let from = std::env::current_exe().context("finding the codexnotifier executable")?;
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
        .with_context(|| format!("copying codexnotifier to {}", dir.display()))?;
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

/// The command Codex runs. One string through the user's shell on macOS and Linux; the Windows
/// form is quoted for cmd/PowerShell. The trust hash covers `command` only.
pub fn hook_command(exe: &Path) -> String {
    if cfg!(windows) {
        format!("\"{}\" hook", exe.to_string_lossy())
    } else {
        format!("{} hook", shell_quote(exe))
    }
}

fn hook_entry(exe: &Path, timeout: u64) -> Value {
    json!({ "type": "command", "command": hook_command(exe), "timeout": timeout })
}

fn command_of(entry: &Value) -> &str {
    entry.get("command").and_then(|c| c.as_str()).unwrap_or("")
}

pub fn is_ours(entry: &Value) -> bool {
    let command = command_of(entry);
    if !command.ends_with(" hook") {
        return false;
    }
    let exe = command
        .trim_end_matches(" hook")
        .trim_matches('\'')
        .trim_matches('"');
    // Both separators: a Windows path in a file read on another platform still counts
    let base = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    base == "codexnotifier" || base == "codexnotifier.exe"
}

// ============================================
// The trust hash
// ============================================

/// What Codex hashes for one of our handlers: `hook_hash` in codex-rs/hooks/src/engine/discovery.rs
/// builds `{ event_name, matcher?, hooks: [normalised handler] }`, where the normalised command
/// handler is `{ type, command, timeout (always set), async }` with `commandWindows` and
/// `statusMessage` left out when absent. Serialised as TOML, converted to JSON with every object's
/// keys sorted, hashed with SHA-256.
pub fn trust_hash(event_label: &str, matcher: Option<&str>, command: &str, timeout: u64) -> String {
    let mut identity = json!({
        "event_name": event_label,
        "hooks": [{ "type": "command", "command": command, "timeout": timeout, "async": false }],
    });
    if let Some(m) = matcher {
        identity["matcher"] = json!(m);
    }
    identity.sort_all_objects();
    let serialized = serde_json::to_vec(&identity).unwrap_or_default();
    let digest = Sha256::digest(serialized);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// The `[hooks.state]` key Codex uses for a handler: the hooks file's path, the event label and
/// the handler's position.
pub fn state_key(hooks_path: &Path, event_label: &str, group: usize, handler: usize) -> String {
    format!("{}:{event_label}:{group}:{handler}", hooks_path.display())
}

/// `(key, hash)` for every entry of ours in `hooks`, by position.
fn our_entries(hooks_path: &Path, hooks: &serde_json::Map<String, Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (event, label, _, _) in EVENTS {
        let Some(groups) = hooks.get(*event).and_then(|g| g.as_array()) else {
            continue;
        };
        for (gi, group) in groups.iter().enumerate() {
            let matcher = group.get("matcher").and_then(|m| m.as_str());
            let Some(list) = group.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for (hi, entry) in list.iter().enumerate() {
                if !is_ours(entry) {
                    continue;
                }
                let timeout = normalised_timeout(label, entry.get("timeout").and_then(|t| t.as_u64()));
                out.push((
                    state_key(hooks_path, label, gi, hi),
                    trust_hash(label, matcher, command_of(entry), timeout),
                ));
            }
        }
    }
    out
}

/// Codex's `normalize_command_hook`: SessionEnd and Interrupt default to 1 s and are clamped to
/// 3 s; everything else defaults to 600 s and has no cap.
fn normalised_timeout(label: &str, timeout: Option<u64>) -> u64 {
    match label {
        "session_end" | "interrupt" => timeout.unwrap_or(1).clamp(1, 3),
        _ => timeout.unwrap_or(600).max(1),
    }
}

// ============================================
// hooks.json
// ============================================

fn load_hooks(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({ "hooks": {} }));
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(json!({ "hooks": {} }));
    }
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))?;
    if !root.is_object() {
        anyhow::bail!("{} is not a JSON object", path.display());
    }
    if !root.get("hooks").is_some_and(|h| h.is_object()) {
        root["hooks"] = json!({});
    }
    Ok(root)
}

fn save_hooks(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let backup = path.with_extension("json.codexnotifier-backup");
        if !backup.exists() {
            let _ = fs::copy(path, &backup);
        }
    }
    let text = serde_json::to_string_pretty(root)?;
    fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Removes our entries from every event. Drops groups and events left empty.
fn strip(hooks: &mut serde_json::Map<String, Value>) {
    for (_event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                list.retain(|entry| !is_ours(entry));
            }
        }
        groups.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|l| !l.is_empty())
                .unwrap_or(true)
        });
    }
    hooks.retain(|_, groups| groups.as_array().map(|g| !g.is_empty()).unwrap_or(true));
}

fn add_ours(hooks: &mut serde_json::Map<String, Value>, exe: &Path) -> Result<()> {
    for (event, _, matcher, timeout) in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .with_context(|| format!("hooks.{event} is not an array"))?;
        let mut group = json!({ "hooks": [hook_entry(exe, *timeout)] });
        if let Some(m) = matcher {
            group["matcher"] = json!(m);
        }
        groups.push(group);
    }
    Ok(())
}

pub fn install() -> Result<InstallReport> {
    let hooks_path = config::codex_hooks_path();
    let config_path = config::codex_config_path();
    let exe = exe_path()?;
    let ours_before = ours_in_file(&hooks_path);
    let before = load_hooks(&hooks_path)?;
    let mut root = before.clone();
    let hooks = root["hooks"]
        .as_object_mut()
        .context("the hooks section of hooks.json is not an object")?;
    strip(hooks);
    add_ours(hooks, &exe)?;
    let entries = our_entries(&hooks_path, hooks);
    // Idempotent: a second run finds its own entries at the end of each list and writes nothing
    let hooks_changed = root != before || !hooks_path.exists();
    if hooks_changed {
        save_hooks(&hooks_path, &root)?;
    }
    let trust_changed = write_trust(&config_path, &hooks_path, &ours_before, &entries)?;
    Ok(InstallReport {
        hooks_path,
        config_path,
        exe,
        changed: hooks_changed || trust_changed,
    })
}

pub fn uninstall() -> Result<PathBuf> {
    let hooks_path = config::codex_hooks_path();
    let ours_before = ours_in_file(&hooks_path);
    let _ = write_trust(&config::codex_config_path(), &hooks_path, &ours_before, &[]);
    if !hooks_path.exists() {
        return Ok(hooks_path);
    }
    let mut root = load_hooks(&hooks_path)?;
    if let Some(hooks) = root["hooks"].as_object_mut() {
        strip(hooks);
    }
    let empty = root["hooks"].as_object().is_some_and(|h| h.is_empty())
        && root.as_object().is_some_and(|o| o.len() == 1);
    if empty {
        // A file holding nothing but an empty hooks section is almost always one we created
        let _ = fs::remove_file(&hooks_path);
    } else {
        save_hooks(&hooks_path, &root)?;
    }
    Ok(hooks_path)
}

pub fn status() -> Result<Status> {
    let hooks_path = config::codex_hooks_path();
    let root = load_hooks(&hooks_path)?;
    let exe = exe_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let trusted = read_trust(&config::codex_config_path());
    let mut installed = Vec::new();
    let mut missing = Vec::new();
    let mut untrusted = Vec::new();
    let mut stale_exe = false;

    let hooks = root["hooks"].as_object();
    let entries = hooks
        .map(|h| our_entries(&hooks_path, h))
        .unwrap_or_default();
    for (event, label, matcher, _) in EVENTS {
        let mut present = false;
        if let Some(groups) = hooks.and_then(|h| h.get(*event)).and_then(|g| g.as_array()) {
            for (gi, group) in groups.iter().enumerate() {
                let group_matcher = group.get("matcher").and_then(|m| m.as_str());
                let matcher_ok = match matcher {
                    Some(m) => group_matcher == Some(m),
                    None => group_matcher
                        .map(|m| m.is_empty() || m == "*")
                        .unwrap_or(true),
                };
                let Some(list) = group.get("hooks").and_then(|h| h.as_array()) else {
                    continue;
                };
                for (hi, entry) in list.iter().enumerate() {
                    if !(matcher_ok && is_ours(entry)) {
                        continue;
                    }
                    present = true;
                    if !exe.is_empty() && !command_of(entry).contains(&exe) {
                        stale_exe = true;
                    }
                    let key = state_key(&hooks_path, label, gi, hi);
                    let expected = entries
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, h)| h.as_str());
                    if trusted.get(&key).map(|s| s.as_str()) != expected {
                        untrusted.push(event.to_string());
                    }
                }
            }
        }
        let label = match matcher {
            Some(m) => format!("{event} ({m})"),
            None => event.to_string(),
        };
        if present {
            installed.push(label);
        } else {
            missing.push(label);
        }
    }
    Ok(Status {
        hooks_path,
        installed,
        missing,
        untrusted,
        stale_exe,
    })
}

// ============================================
// config.toml: [hooks.state."<key>"]
// ============================================

fn load_doc(path: &Path) -> Result<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", path.display()))
}

/// Replaces the trust entries for our handlers (`remove`: their keys before this change, by
/// position) with `entries`. Entries for other people's handlers in the same file, and everything
/// else in config.toml, stay as they are. Returns whether config.toml was written: a file that
/// already holds exactly these entries is left alone.
fn write_trust(
    config_path: &Path,
    hooks_path: &Path,
    remove: &[String],
    entries: &[(String, String)],
) -> Result<bool> {
    let mut doc = load_doc(config_path)?;
    let original = doc.to_string();
    let had_hooks = doc.get("hooks").is_some();
    let hooks = doc
        .entry("hooks")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("the hooks section of config.toml is not a table")?;
    if !had_hooks {
        hooks.set_implicit(true);
    }
    let had_state = hooks.get("state").is_some();
    let state = hooks
        .entry("state")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("hooks.state in config.toml is not a table")?;
    if !had_state {
        state.set_implicit(true);
    }
    let prefix = format!("{}:", hooks_path.display());
    let current: Vec<String> = state.iter().map(|(k, _)| k.to_string()).collect();
    for key in current {
        let ours = remove.contains(&key) || entries.iter().any(|(k, _)| *k == key);
        if key.starts_with(&prefix) && ours {
            state.remove(&key);
        }
    }
    for (key, hash) in entries {
        let mut entry = Table::new();
        entry["trusted_hash"] = toml_edit::value(hash.as_str());
        state.insert(key, Item::Table(entry));
    }
    if state.is_empty() && !had_state {
        hooks.remove("state");
    }
    if hooks.is_empty() && !had_hooks {
        doc.remove("hooks");
    }
    if doc.is_empty() && !config_path.exists() {
        return Ok(false);
    }
    let text = doc.to_string();
    if config_path.exists() && text == original {
        return Ok(false);
    }
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config_path, text).with_context(|| format!("writing {}", config_path.display()))?;
    Ok(true)
}

/// The keys of our handlers as hooks.json on disk has them now.
fn ours_in_file(hooks_path: &Path) -> Vec<String> {
    load_hooks(hooks_path)
        .ok()
        .and_then(|root| {
            root["hooks"]
                .as_object()
                .map(|h| our_entries(hooks_path, h).into_iter().map(|(k, _)| k).collect())
        })
        .unwrap_or_default()
}

/// `key → trusted_hash` for every entry in `[hooks.state]`.
fn read_trust(config_path: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(doc) = load_doc(config_path) else {
        return out;
    };
    let Some(state) = doc
        .get("hooks")
        .and_then(|h| h.as_table_like())
        .and_then(|h| h.get("state"))
        .and_then(|s| s.as_table_like())
    else {
        return out;
    };
    for (key, item) in state.iter() {
        if let Some(hash) = item
            .as_table_like()
            .and_then(|t| t.get("trusted_hash"))
            .and_then(|v| v.as_str())
        {
            out.insert(key.to_string(), hash.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)] // Homebrew paths are Unix paths
    fn homebrew_cellar_maps_to_the_bin_link() {
        let root = std::env::temp_dir().join(format!("cxn-brew-{}", std::process::id()));
        let cellar = root.join("Cellar/codexnotifier/0.1.0/bin");
        fs::create_dir_all(&cellar).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(cellar.join("codexnotifier"), "").unwrap();
        fs::write(root.join("bin/codexnotifier"), "").unwrap();
        assert_eq!(
            homebrew_bin(&cellar.join("codexnotifier")),
            Some(root.join("bin/codexnotifier"))
        );
        assert_eq!(homebrew_bin(&root.join("elsewhere/codexnotifier")), None);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn trust_hash_is_stable_and_sorted() {
        let a = trust_hash("stop", None, "/usr/local/bin/codexnotifier hook", 700);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64);
        assert_eq!(a, trust_hash("stop", None, "/usr/local/bin/codexnotifier hook", 700));
        assert_ne!(a, trust_hash("stop", Some("Bash"), "/usr/local/bin/codexnotifier hook", 700));
        assert_ne!(a, trust_hash("stop", None, "/usr/local/bin/codexnotifier hook", 600));
    }

    #[test]
    fn recognises_our_entries() {
        assert!(is_ours(&json!({ "type": "command", "command": "/opt/homebrew/bin/codexnotifier hook" })));
        assert!(is_ours(&json!({ "type": "command", "command": "'/Users/me/my bin/codexnotifier' hook" })));
        assert!(is_ours(&json!({ "type": "command", "command": "\"C:\\Users\\me\\codexnotifier.exe\" hook" })));
        assert!(!is_ours(&json!({ "type": "command", "command": "ccn hook" })));
        assert!(!is_ours(&json!({ "type": "command", "command": "codexnotifier doctor" })));
    }

    #[test]
    fn install_writes_hooks_and_trust_and_uninstall_restores() {
        let root = std::env::temp_dir().join(format!("cxn-hooks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let hooks_path = root.join("hooks.json");
        let config_path = root.join("config.toml");
        fs::write(
            &hooks_path,
            r#"{"description":"mine","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        )
        .unwrap();
        let original_config =
            "model = \"gpt-5.4\"\n\n[hooks.state.\"other\"]\ntrusted_hash = \"sha256:x\"\n";
        fs::write(&config_path, original_config).unwrap();

        let exe = Path::new("/usr/local/bin/codexnotifier");
        let before = ours_in_file(&hooks_path);
        assert!(before.is_empty());
        let mut root_json = load_hooks(&hooks_path).unwrap();
        let hooks = root_json["hooks"].as_object_mut().unwrap();
        strip(hooks);
        add_ours(hooks, exe).unwrap();
        let entries = our_entries(&hooks_path, hooks);
        assert_eq!(entries.len(), EVENTS.len());
        // Our Stop entry sits after the user's own group
        let stop = entries.iter().find(|(k, _)| k.ends_with(":stop:1:0")).unwrap();
        assert!(entries.iter().any(|(k, _)| k.ends_with(":session_start:0:0")));
        save_hooks(&hooks_path, &root_json).unwrap();
        write_trust(&config_path, &hooks_path, &before, &entries).unwrap();

        let text = fs::read_to_string(&config_path).unwrap();
        assert!(text.starts_with("model = \"gpt-5.4\"\n"), "{text}");
        assert!(text.contains("[hooks.state.\"other\"]"));
        assert!(text.contains(&format!(":stop:1:0\"]\ntrusted_hash = \"{}\"", stop.1)), "{text}");
        assert_eq!(read_trust(&config_path).len(), EVENTS.len() + 1);
        let saved = fs::read_to_string(&hooks_path).unwrap();
        assert!(saved.contains("\"description\": \"mine\""));
        assert!(saved.contains("echo hi"));

        // A second install finds the same positions and leaves one entry each
        let before = ours_in_file(&hooks_path);
        assert_eq!(before.len(), EVENTS.len());
        write_trust(&config_path, &hooks_path, &before, &entries).unwrap();
        assert_eq!(read_trust(&config_path).len(), EVENTS.len() + 1);

        // Uninstall leaves the user's hook and trust entry, and removes ours
        write_trust(&config_path, &hooks_path, &before, &[]).unwrap();
        let mut root_json = load_hooks(&hooks_path).unwrap();
        strip(root_json["hooks"].as_object_mut().unwrap());
        save_hooks(&hooks_path, &root_json).unwrap();
        let text = fs::read_to_string(&config_path).unwrap();
        assert_eq!(text, original_config);
        let saved: Value = serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        assert_eq!(saved["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(saved["hooks"].get("SessionStart").is_none());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn trust_written_to_a_config_without_a_hooks_section() {
        let root = std::env::temp_dir().join(format!("cxn-trust-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.toml");
        fs::write(&config_path, "model = \"gpt-5.4\"\n").unwrap();
        let hooks_path = root.join("hooks.json");
        let key = state_key(&hooks_path, "stop", 0, 0);
        write_trust(&config_path, &hooks_path, &[], &[(key.clone(), "sha256:abc".into())]).unwrap();
        let text = fs::read_to_string(&config_path).unwrap();
        assert!(text.starts_with("model = \"gpt-5.4\"\n"), "{text}");
        assert!(!text.contains("\n[hooks]\n"), "no empty header: {text}");
        assert_eq!(read_trust(&config_path).get(&key).map(String::as_str), Some("sha256:abc"));
        write_trust(&config_path, &hooks_path, &[key], &[]).unwrap();
        assert_eq!(fs::read_to_string(&config_path).unwrap(), "model = \"gpt-5.4\"\n");
        fs::remove_dir_all(root).ok();
    }
}
