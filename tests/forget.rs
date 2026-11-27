//! `antigravity-notifier forget` end to end: the real binary against a one-request fake server, in a temp home.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    root: PathBuf,
    /// What `ANTIGRAVITY_NOTIFIER_LANG` says; English unless a test is about the language
    lang: &'static str,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "antigravity-notifier-forget-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("gemini/config")).unwrap();
        std::fs::create_dir_all(root.join("antigravity-notifier")).unwrap();
        std::fs::write(
            root.join("antigravity-notifier/config.toml"),
            "client_id = \"test-computer\"\n",
        )
        .unwrap();
        std::fs::write(root.join("antigravity-notifier/credentials"), "test-token\n").unwrap();
        // The user's own hook group, which must survive everything
        std::fs::write(
            root.join("gemini/config/hooks.json"),
            r#"{"mine":{"Stop":[{"type":"command","command":"echo hi"}]}}"#,
        )
        .unwrap();
        Sandbox { root, lang: "en" }
    }

    fn app_dir(&self) -> PathBuf {
        self.root.join("antigravity-notifier")
    }

    /// As the brand ships: no licences sold on the website (`brand::WEB_LICENCES` is off)
    fn run(&self, server: &str, args: &[&str]) -> Output {
        self.run_with_licences(server, args, false)
    }

    /// As a brand that sells web licences behaves (ccn, codex-notifier, and this one if the
    /// switch is ever turned on)
    fn run_selling_licences(&self, server: &str, args: &[&str]) -> Output {
        self.run_with_licences(server, args, true)
    }

    fn run_with_licences(&self, server: &str, args: &[&str], sold: bool) -> Output {
        Command::new(env!("CARGO_BIN_EXE_antigravity-notifier"))
            .args(args)
            .env("ANTIGRAVITY_NOTIFIER_TEST_WEB_LICENCES", if sold { "1" } else { "0" })
            .env("HOME", &self.root)
            .env("ANTIGRAVITY_NOTIFIER_HOME", self.app_dir())
            .env("ANTIGRAVITY_NOTIFIER_GEMINI_HOME", self.root.join("gemini"))
            .env("ANTIGRAVITY_NOTIFIER_SERVER_URL", server)
            .env("ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE", "file")
            .env("ANTIGRAVITY_NOTIFIER_NOTIFY", "off")
            .env("ANTIGRAVITY_NOTIFIER_NO_ANALYTICS", "1")
            .env("ANTIGRAVITY_NOTIFIER_LANG", self.lang)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Answers one request with `status` and hands back its request line and headers.
fn fake_server(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let reply = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        // Close gracefully: Windows resets a connection closed with unread data, and antigravity-notifier then
        // sees "connection aborted" instead of the reply
        let _ = stream.shutdown(std::net::Shutdown::Write);
        request
    });
    (url, handle)
}

/// The whole request: headers, then as much body as Content-Length says. Reading all of it before
/// replying matters on Windows, which resets a socket closed with unread bytes.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&data).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                return text;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&data).to_string(),
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn settings_hooks_gone(sandbox: &Sandbox) -> bool {
    let hooks =
        std::fs::read_to_string(Path::new(&sandbox.root).join("gemini/config/hooks.json"))
            .unwrap();
    !hooks.contains("antigravity-notifier") && hooks.contains("echo hi")
}

#[test]
fn erases_on_the_server_then_removes_everything_local() {
    let sandbox = Sandbox::new("ok");
    let (url, server) = fake_server(200, r#"{"erased":true,"purged":true}"#);
    let out = sandbox.run(&url, &["forget", "--yes"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let request = server.join().unwrap();
    assert!(request.starts_with("DELETE /api/clients/me "), "{request}");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-token"),
        "{request}"
    );
    assert!(
        request.to_ascii_lowercase().contains("accept-language: en"),
        "{request}"
    );
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("Erased this computer and its pairings from the server."),
        "{stdout}"
    );
    // No licences are sold for this brand, so nothing says a word about one
    assert!(!stdout.to_lowercase().contains("licence"), "{stdout}");
    assert!(!sandbox.app_dir().exists());
    assert!(settings_hooks_gone(&sandbox));
}

#[test]
fn where_licences_are_sold_forget_says_how_to_erase_one() {
    let sandbox = Sandbox::new("ok-licences");
    let (url, server) = fake_server(200, r#"{"success":true,"data":{"deleted":true}}"#);
    let out = sandbox.run_selling_licences(&url, &["forget", "--yes"]);
    server.join().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("antigravitynotifier.com/licence") && stdout.contains("Erase my licence"),
        "{stdout}"
    );
}

/// The OS language (or `ANTIGRAVITY_NOTIFIER_LANG`) reaches the server as `Accept-Language`, mapped onto one of
/// the supported tags, whatever locales/*.yml files exist.
#[test]
fn asks_the_server_in_the_users_language() {
    let mut sandbox = Sandbox::new("lang");
    sandbox.lang = "pt_PT.UTF-8";
    let (url, server) = fake_server(200, r#"{"erased":true,"purged":true}"#);
    let out = sandbox.run(&url, &["forget", "--yes"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let request = server.join().unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("accept-language: pt-br"),
        "{request}"
    );
}

#[test]
fn a_401_means_already_gone_so_it_still_cleans_up_locally() {
    let sandbox = Sandbox::new("gone");
    let (url, server) = fake_server(401, r#"{"error":{"message":"unknown"}}"#);
    let out = sandbox.run(&url, &["forget", "-y"]);
    server.join().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("already forgotten"));
    assert!(!sandbox.app_dir().exists());
}

#[test]
fn an_unreachable_server_keeps_the_local_credentials() {
    let sandbox = Sandbox::new("offline");
    // A port nothing is listening on
    let url = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let out = sandbox.run(&url, &["forget", "--yes"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("Nothing was removed from this computer"),
        "{}",
        text(&out.stderr)
    );
    assert!(sandbox.app_dir().join("credentials").exists());
    assert!(sandbox.app_dir().join("config.toml").exists());
}

#[test]
fn a_server_error_keeps_the_local_credentials() {
    let sandbox = Sandbox::new("500");
    let (url, server) = fake_server(500, r#"{"error":{"message":"boom"}}"#);
    let out = sandbox.run(&url, &["forget", "--yes"]);
    server.join().unwrap();
    assert!(!out.status.success());
    assert!(sandbox.app_dir().join("credentials").exists());
}

#[test]
fn refuses_without_yes_when_there_is_no_terminal() {
    let sandbox = Sandbox::new("noyes");
    // If this reached the network the connection would fail differently, and the files would still be asserted
    let out = sandbox.run("http://127.0.0.1:9", &["forget"]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("--yes"), "{}", text(&out.stderr));
    assert!(sandbox.app_dir().join("credentials").exists());
}

fn unreachable_url() -> String {
    // A port nothing is listening on
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

/// `ANTIGRAVITY_NOTIFIER_HOME` may point at a folder that holds other things: only antigravity-notifier's own files go.
#[test]
fn removes_only_its_own_files_from_a_shared_folder() {
    let sandbox = Sandbox::new("shared");
    let dir = sandbox.app_dir();
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    std::fs::create_dir_all(dir.join("sessions")).unwrap();
    std::fs::write(dir.join("sessions/plan.txt"), "mine too").unwrap();
    std::fs::write(
        dir.join("sessions/abc.json"),
        r#"{"session_id":"abc","updated_at":1}"#,
    )
    .unwrap();
    std::fs::write(dir.join("unpaired.json"), "{}").unwrap();
    std::fs::write(dir.join(".config.toml.123-abcdef12.tmp"), "half").unwrap();
    let (url, server) = fake_server(200, r#"{"erased":true,"purged":true}"#);
    let out = sandbox.run(&url, &["forget", "--yes"]);
    server.join().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let mut left: Vec<String> = walk(&dir);
    left.sort();
    assert_eq!(left, vec!["notes.txt", "sessions/plan.txt"]);
    assert!(
        text(&out.stdout).contains("left in place"),
        "{}",
        text(&out.stdout)
    );
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            out.extend(
                walk(&entry.path())
                    .into_iter()
                    .map(|n| format!("{name}/{n}")),
            );
        } else {
            out.push(name);
        }
    }
    out
}

/// Offline, uninstall still removes everything, but says the licence slot may still be held and
/// how to free it, since the token it would need is gone.
#[test]
fn uninstall_offline_says_the_licence_slot_was_not_freed() {
    let sandbox = Sandbox::new("uninstall-offline");
    let out = sandbox.run_selling_licences(&unreachable_url(), &["uninstall"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("Couldn't free this computer's licence slot")
            && stderr.contains("antigravitynotifier.com/licence"),
        "{stderr}"
    );
    assert!(!sandbox.app_dir().exists());
    assert!(settings_hooks_gone(&sandbox));
    // The binary is left to whatever installed it, with the command to remove it
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("antigravity-notifier itself is still installed"),
        "{stdout}"
    );
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_antigravity-notifier"));
    assert!(exe.exists());
}

/// No licences are sold for this brand: uninstall, even offline, has no slot to talk about.
#[test]
fn uninstall_says_nothing_of_licences_where_none_are_sold() {
    let sandbox = Sandbox::new("uninstall-plain");
    let out = sandbox.run(&unreachable_url(), &["uninstall"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let said = format!("{}{}", text(&out.stdout), text(&out.stderr)).to_lowercase();
    assert!(!said.contains("licence") && !said.contains("unlock"), "{said}");
    assert!(!sandbox.app_dir().exists());
    assert!(settings_hooks_gone(&sandbox));
}

/// A server that has already forgotten the computer has no slot for it: nothing to report.
#[test]
fn uninstall_after_the_server_forgot_the_computer_is_quiet_about_the_slot() {
    let sandbox = Sandbox::new("uninstall-gone");
    let (url, server) = fake_server(401, r#"{"error":{"code":"UNAUTHORIZED","message":"no"}}"#);
    let out = sandbox.run_selling_licences(&url, &["uninstall"]);
    let request = server.join().unwrap();
    assert!(
        request.starts_with("POST /api/clients/me/licence/deactivate "),
        "{request}"
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        !text(&out.stderr).contains("licence slot"),
        "{}",
        text(&out.stderr)
    );
    assert!(!sandbox.app_dir().exists());
}
