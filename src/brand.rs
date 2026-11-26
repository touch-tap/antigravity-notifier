//! Every name and address that belongs to the Antigravity Notifier brand, in one place. Several
//! are placeholders until the product launches: the list in README.md ("Before a release") says
//! which. The install scripts, package manifests and locale files carry their own copies of the
//! website address (grep for `antigravitynotifier.com`).

/// The shared Worker that also serves Claude Code Notifier and Notifier for Codex.
/// PLACEHOLDER: *.workers.dev is blocked in mainland China, so this becomes a custom domain
/// (api.antigravitynotifier.com) once the domain is registered.
pub const DEFAULT_SERVER_URL: &str = "https://ccnotifier-api.touchtap.workers.dev";

/// What this client calls itself when it registers (`clientKind`) and on every request (`X-App`).
/// The server must know both before pairing works.
pub const CLIENT_KIND: &str = "antigravity_cli";
pub const APP_HEADER: &str = "antigravity";

/// PLACEHOLDER: the domain is not registered yet.
pub const WEBSITE: &str = "antigravitynotifier.com";

/// Where a licence is managed or erased: `<WEBSITE>/licence`
pub fn licence_page() -> String {
    format!("{WEBSITE}/licence")
}

/// Where the other ways to install are listed: `<WEBSITE>/cli`
pub fn cli_page() -> String {
    format!("{WEBSITE}/cli")
}

/// PLACEHOLDER: the repository touch-tap/antigravity-notifier does not exist yet, so `upgrade`
/// finds no release.

pub const GITHUB_LATEST: &str =
    "https://api.github.com/repos/touch-tap/antigravity-notifier/releases/latest";
pub const RELEASES_BASE: &str =
    "https://github.com/touch-tap/antigravity-notifier/releases/download";

/// Our sister app for Claude Code, which crosspromo.rs suggests to someone who also uses it.
/// These are live; only the `ref` tags are new.
pub mod claude_code {
    pub const UNIX_COMMAND: &str = "curl -fsSL https://claudecodenotifier.com/install.sh | sh";
    pub const WINDOWS_COMMAND: &str =
        "irm https://claudecodenotifier.com/install.ps1 | iex; ccn pair";
    /// The website, whose buttons take a phone to the App Store or Google Play
    pub const URL: &str = "https://claudecodenotifier.com/?ref=antigravity-cli";
    /// The QR code and link after `antigravity-notifier pair`
    pub const PAIR_URL: &str = "https://claudecodenotifier.com/get?ref=antigravity-pair";
    /// Where a click on the SessionStart notification goes
    pub const NOTIFICATION_URL: &str =
        "https://claudecodenotifier.com/?ref=antigravity-notification";
}
