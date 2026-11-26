//! `antigravity-notifier upgrade` end to end: the real binary, copied into a temp bin directory, against a fake
//! server that serves `/api/cli/latest`, a release archive holding a tiny script called `antigravity-notifier`,
//! and its SHA256SUMS.

#![cfg(unix)] // the release archive is a tar.gz of a shell script; Windows gets a zip of an exe

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const NEW_VERSION: &str = "9.9.9";
const SCRIPT: &str = "#!/bin/sh\necho \"antigravity-notifier 9.9.9\"\n";
/// The key the fake releases are signed with, and one antigravity-notifier doesn't trust
const TEST_SEED: [u8; 32] = [7; 32];
const OTHER_SEED: [u8; 32] = [9; 32];

fn test_key(seed: [u8; 32]) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// How the fake release signs its SHA256SUMS.
#[derive(Clone, Copy)]
enum Sig {
    Good,
    WrongKey,
    Missing,
}

fn asset_name() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("antigravity-notifier-{arch}-apple-darwin.tar.gz"),
        _ => format!("antigravity-notifier-{arch}-unknown-linux-musl.tar.gz"),
    }
}

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A home with antigravity-notifier's config, Antigravity's hooks file (pointing at the sandbox binary) and a
    /// copy of the real antigravity-notifier at `bin/antigravity-notifier`, or at `exe_rel` when given.
    fn new(name: &str, exe_rel: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "antigravity-notifier-update-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("gemini/config")).unwrap();
        std::fs::create_dir_all(root.join("antigravity-notifier")).unwrap();
        let exe = root.join(exe_rel);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_antigravity-notifier"), &exe).unwrap();
        // As `antigravity-notifier setup` writes it: the canonical path (macOS puts temp dirs under /private)
        let exe = exe.canonicalize().unwrap();
        std::fs::write(
            root.join("antigravity-notifier/config.toml"),
            "auto_update = true\n",
        )
        .unwrap();
        let hook = format!("{} hook Stop", exe.display());
        std::fs::write(
            root.join("gemini/config/hooks.json"),
            format!(
                r#"{{"antigravity-notifier":{{"Stop":[{{"type":"command","command":"{hook}","timeout":700}}]}}}}"#
            ),
        )
        .unwrap();
        Sandbox { root }
    }

    fn exe(&self) -> PathBuf {
        self.root.join("bin/antigravity-notifier").canonicalize().unwrap()
    }

    fn run_exe(&self, exe: &PathBuf, server: &str, args: &[&str]) -> Output {
        Command::new(exe)
            .args(args)
            .env("HOME", &self.root)
            .env("ANTIGRAVITY_NOTIFIER_HOME", self.root.join("antigravity-notifier"))
            .env("ANTIGRAVITY_NOTIFIER_GEMINI_HOME", self.root.join("gemini"))
            .env("ANTIGRAVITY_NOTIFIER_SERVER_URL", server)
            .env("ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE", "file")
            .env("ANTIGRAVITY_NOTIFIER_NOTIFY", "off")
            .env("ANTIGRAVITY_NOTIFIER_NO_ANALYTICS", "1")
            // The server's downloadBase is ignored (only GitHub's releases are trusted), so the
            // fake release is reached the way CI's is
            .env("ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE", format!("{server}/dl"))
            // A debug build also trusts this key, so the fake release can be signed here
            .env(
                "ANTIGRAVITY_NOTIFIER_UPDATE_PUBLIC_KEY",
                hex(test_key(TEST_SEED).verifying_key().as_bytes()),
            )
            .env_remove("ANTIGRAVITY_NOTIFIER_INSTALL_METHOD")
            .env_remove("ANTIGRAVITY_NOTIFIER_EPHEMERAL")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn run(&self, server: &str, args: &[&str]) -> Output {
        self.run_exe(&self.exe(), server, args)
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.root.join("antigravity-notifier/config.toml")).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A release: the archive holding the script, and the SHA256SUMS line for it.
fn release(dir: &Path, correct_checksum: bool) -> (Vec<u8>, String) {
    let stage = dir.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("antigravity-notifier"), SCRIPT).unwrap();
    let archive = dir.join(asset_name());
    let status = Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(&stage)
        .arg("antigravity-notifier")
        .status()
        .unwrap();
    assert!(status.success(), "tar failed");
    let bytes = std::fs::read(&archive).unwrap();
    let hash = if correct_checksum {
        use sha2::Digest;
        sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    } else {
        "0".repeat(64)
    };
    (bytes, format!("{hash}  {}\n", asset_name()))
}

type Routes = HashMap<String, (&'static str, Vec<u8>)>;

/// Serves `routes` by path for as long as the test runs, remembering what was asked for.
fn serve(build: impl FnOnce(&str) -> Routes) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes = build(&url);
    let requested = Arc::new(Mutex::new(Vec::new()));
    let seen = requested.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let head = String::from_utf8_lossy(&request).to_string();
            let path = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push(path.clone());
            let reply = match routes.get(&path) {
                Some((ctype, body)) => {
                    let mut r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(body);
                    r
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&reply);
        }
    });
    (url, requested)
}

/// A server whose latest release is `version`, with the archive and checksums under `/dl`.
fn release_server(
    dir: &Path,
    version: &str,
    correct_checksum: bool,
) -> (String, Arc<Mutex<Vec<String>>>) {
    release_server_signed(dir, version, correct_checksum, Sig::Good)
}

fn release_server_signed(
    dir: &Path,
    version: &str,
    correct_checksum: bool,
    sig: Sig,
) -> (String, Arc<Mutex<Vec<String>>>) {
    use ed25519_dalek::Signer;
    let (archive, sums) = release(dir, correct_checksum);
    let signature = match sig {
        Sig::Good => Some(test_key(TEST_SEED).sign(sums.as_bytes())),
        Sig::WrongKey => Some(test_key(OTHER_SEED).sign(sums.as_bytes())),
        Sig::Missing => None,
    }
    .map(|s| format!("{}\n", hex(&s.to_bytes())));
    let version = version.to_string();
    serve(move |url| {
        let latest = format!(
            r#"{{"version":"{version}","tag":"v{version}","publishedAt":1761800000,"downloadBase":"{url}/evil","assets":["{}","SHA256SUMS"]}}"#,
            asset_name()
        );
        let mut routes = HashMap::from([
            (
                "/api/cli/latest".to_string(),
                ("application/json", latest.into_bytes()),
            ),
            (
                format!("/dl/{}", asset_name()),
                ("application/gzip", archive),
            ),
            (
                "/dl/SHA256SUMS".to_string(),
                ("text/plain", sums.into_bytes()),
            ),
        ]);
        if let Some(signature) = signature {
            routes.insert(
                "/dl/SHA256SUMS.sig".to_string(),
                ("text/plain", signature.into_bytes()),
            );
        }
        routes
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn replaces_the_binary_with_the_verified_release() {
    let sandbox = Sandbox::new("ok", "bin/antigravity-notifier");
    let original = std::fs::read(sandbox.exe()).unwrap();
    let (url, requested) = release_server(&sandbox.root, NEW_VERSION, true);

    let out = sandbox.run(&url, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        format!("Updated antigravity-notifier {VERSION} → {NEW_VERSION}.")
    );

    let requested = requested.lock().unwrap().clone();
    assert_eq!(
        requested,
        vec![
            "/api/cli/latest".to_string(),
            format!("/dl/{}", asset_name()),
            "/dl/SHA256SUMS".to_string(),
            "/dl/SHA256SUMS.sig".to_string(),
        ]
    );

    // The release's antigravity-notifier is now the installed one, the old binary is gone, nothing is left beside it
    let installed = std::fs::read(sandbox.exe()).unwrap();
    assert_eq!(text(&installed), SCRIPT);
    assert_ne!(installed, original);
    assert!(!sandbox.root.join("bin/antigravity-notifier.new").exists());
    assert!(!sandbox.root.join("bin/antigravity-notifier.old").exists());
    assert_eq!(
        std::fs::read_dir(sandbox.root.join("bin")).unwrap().count(),
        1
    );
    let version = Command::new(sandbox.exe())
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(text(&version.stdout).trim(), "antigravity-notifier 9.9.9");

    // The hooks already pointed at this path, so hooks.json was not rewritten
    let settings = std::fs::read_to_string(sandbox.root.join("gemini/config/hooks.json")).unwrap();
    assert!(settings.contains(&format!("{} hook Stop", sandbox.exe().display())));
    assert!(
        !sandbox
            .root
            .join("gemini/config/hooks.json.antigravity-notifier-backup")
            .exists()
    );
}

#[test]
fn a_bad_checksum_leaves_the_original_untouched() {
    let sandbox = Sandbox::new("badsum", "bin/antigravity-notifier");
    let original = std::fs::read(sandbox.exe()).unwrap();
    let (url, _) = release_server(&sandbox.root, NEW_VERSION, false);

    let out = sandbox.run(&url, &["upgrade"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("checksum mismatch"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(std::fs::read(sandbox.exe()).unwrap(), original);
    assert!(!sandbox.root.join("bin/antigravity-notifier.new").exists());
    let still_runs = sandbox.run(&url, &["--version"]);
    assert_eq!(
        text(&still_runs.stdout).trim(),
        format!("antigravity-notifier {VERSION}")
    );
}

#[test]
fn says_so_when_already_the_latest() {
    let sandbox = Sandbox::new("latest", "bin/antigravity-notifier");
    let original = std::fs::read(sandbox.exe()).unwrap();
    let (url, requested) = release_server(&sandbox.root, VERSION, true);

    let out = sandbox.run(&url, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        format!("antigravity-notifier {VERSION} is the latest.")
    );
    assert_eq!(std::fs::read(sandbox.exe()).unwrap(), original);
    assert_eq!(requested.lock().unwrap().len(), 1, "nothing was downloaded");

    let quiet = sandbox.run(&url, &["update", "--quiet"]);
    assert!(quiet.status.success());
    assert!(quiet.stdout.is_empty(), "{}", text(&quiet.stdout));
}

#[test]
fn check_only_reports_and_exits_10_when_newer() {
    let sandbox = Sandbox::new("check", "bin/antigravity-notifier");
    let original = std::fs::read(sandbox.exe()).unwrap();
    let (url, requested) = release_server(&sandbox.root, NEW_VERSION, true);

    let out = sandbox.run(&url, &["upgrade", "--check"]);
    assert_eq!(out.status.code(), Some(10), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "antigravity-notifier {NEW_VERSION} is available (you have {VERSION})"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("Update: antigravity-notifier upgrade"),
        "{stdout}"
    );
    assert_eq!(std::fs::read(sandbox.exe()).unwrap(), original);
    assert_eq!(requested.lock().unwrap().len(), 1, "nothing was downloaded");

    let (url, _) = release_server(&sandbox.root, VERSION, true);
    let out = sandbox.run(&url, &["upgrade", "--check"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn a_homebrew_install_is_told_to_brew_upgrade_and_the_background_run_records_it() {
    let sandbox = Sandbox::new(
        "brew",
        "homebrew/Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier",
    );
    let exe = sandbox
        .root
        .join("homebrew/Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier");
    let original = std::fs::read(&exe).unwrap();
    let (url, requested) = release_server(&sandbox.root, NEW_VERSION, true);

    let out = sandbox.run_exe(&exe, &url, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        format!(
            "antigravity-notifier {NEW_VERSION} is available: brew upgrade touch-tap/tap/antigravity-notifier"
        )
    );
    assert_eq!(std::fs::read(&exe).unwrap(), original);
    assert!(!sandbox.config().contains("update_available"));

    let out = sandbox.run_exe(&exe, &url, &["upgrade", "--quiet", "--background"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(
        sandbox
            .config()
            .contains(&format!("update_available = \"{NEW_VERSION}\"")),
        "{}",
        sandbox.config()
    );
    assert_eq!(std::fs::read(&exe).unwrap(), original);
    assert_eq!(
        requested.lock().unwrap().len(),
        2,
        "only the version was asked for"
    );

    // Once brew has done it, the next background run clears the note
    let (url, _) = release_server(&sandbox.root, VERSION, true);
    let out = sandbox.run_exe(&exe, &url, &["upgrade", "--quiet", "--background"]);
    assert!(out.status.success());
    assert!(!sandbox.config().contains("update_available"));
}

#[test]
fn auto_update_is_a_local_setting() {
    let sandbox = Sandbox::new("setting", "bin/antigravity-notifier");
    let out = sandbox.run(
        "http://127.0.0.1:9",
        &["config", "set", "auto_update", "off"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("auto_update                off"));
    assert!(sandbox.config().contains("auto_update = false"));
    let out = sandbox.run(
        "http://127.0.0.1:9",
        &["config", "set", "auto_update", "maybe"],
    );
    assert!(!out.status.success());
}

#[test]
#[cfg(unix)]
fn an_intel_mac_homebrew_link_is_left_to_brew() {
    // As Homebrew lays it out under /usr/local: the binary in the Cellar, a relative symlink in
    // bin, and hooks and the shell running the link. Nothing in the link's path says Homebrew.
    let sandbox = Sandbox::new(
        "brewlink",
        "usr/local/Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier",
    );
    let cellar = sandbox
        .root
        .join("usr/local/Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier");
    let link = sandbox.root.join("usr/local/bin/antigravity-notifier");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../Cellar/antigravity-notifier/0.1.0/bin/antigravity-notifier", &link).unwrap();
    let original = std::fs::read(&cellar).unwrap();
    let (url, _) = release_server(&sandbox.root, NEW_VERSION, true);

    let out = sandbox.run_exe(&link, &url, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout).trim(),
        format!(
            "antigravity-notifier {NEW_VERSION} is available: brew upgrade touch-tap/tap/antigravity-notifier"
        )
    );

    let out = sandbox.run_exe(&link, &url, &["upgrade", "--quiet", "--background"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "brew's link is still a link"
    );
    assert_eq!(std::fs::read(&cellar).unwrap(), original);
    assert!(
        sandbox
            .config()
            .contains(&format!("update_available = \"{NEW_VERSION}\"")),
        "{}",
        sandbox.config()
    );
}

#[test]
fn a_second_upgrader_leaves_the_binary_to_the_first() {
    let sandbox = Sandbox::new("locked", "bin/antigravity-notifier");
    let original = std::fs::read(sandbox.exe()).unwrap();
    let (url, requested) = release_server(&sandbox.root, NEW_VERSION, true);
    // Another upgrader, started a moment ago, holds the lock
    let lock = sandbox.root.join("antigravity-notifier/update.lock");
    std::fs::write(&lock, "1\n").unwrap();

    let out = sandbox.run(&url, &["upgrade"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("already running"),
        "{}",
        text(&out.stderr)
    );
    let out = sandbox.run(&url, &["upgrade", "--quiet", "--background"]);
    assert!(out.status.success(), "the background run just stops");
    assert_eq!(std::fs::read(sandbox.exe()).unwrap(), original);
    assert!(
        requested
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.starts_with("/dl/")),
        "nothing was downloaded"
    );
    assert!(lock.exists(), "the other upgrader's lock is left alone");

    // Once it is done, the next upgrade goes ahead and releases the lock after
    std::fs::remove_file(&lock).unwrap();
    let out = sandbox.run(&url, &["upgrade"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&std::fs::read(sandbox.exe()).unwrap()), SCRIPT);
    assert!(!lock.exists());
}

#[test]
fn refuses_a_release_signed_by_another_key_or_not_at_all() {
    for sig in [Sig::WrongKey, Sig::Missing] {
        let sandbox = Sandbox::new("unsigned", "bin/antigravity-notifier");
        let original = std::fs::read(sandbox.exe()).unwrap();
        let (url, requested) = release_server_signed(&sandbox.root, NEW_VERSION, true, sig);

        let out = sandbox.run(&url, &["upgrade"]);
        assert!(!out.status.success(), "an unsigned release was installed");
        // Nothing was replaced, and the archive was never unpacked into place
        assert_eq!(std::fs::read(sandbox.exe()).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(sandbox.root.join("bin")).unwrap().count(),
            1
        );
        assert!(
            requested
                .lock()
                .unwrap()
                .contains(&"/dl/SHA256SUMS.sig".to_string())
        );
    }
}
