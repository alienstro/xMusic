//! How this copy of xmusic was installed, and therefore how to replace it.
//!
//! Nothing here upgrades anything on its own. It works out which installer owns
//! the running binary and hands back the commands that installer would use, so
//! the decision to run them stays with the caller and the user sees exactly the
//! output they would have seen typing those commands themselves.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Where a cargo install builds from. The default branch rather than the newest
/// tag: a cargo install is already a build from source, and someone who chose it
/// over Homebrew is usually after what is on main.
const REPOSITORY: &str = "https://github.com/alienstro/xMusic.git";
const LATEST_RELEASE: &str = "https://api.github.com/repos/alienstro/xMusic/releases/latest";
const TAGS: &str = "https://api.github.com/repos/alienstro/xMusic/tags?per_page=100";
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The two crates, in the order they have to be installed: both binaries end up
/// in one directory, and the client finds the daemon beside itself.
const CRATES: &[&str] = &["xmusic-player", "xmusic-tui"];

#[derive(Debug)]
pub enum Source {
    /// Homebrew owns it. The keg name is what `brew upgrade` takes, and it is
    /// the short name even when the formula came from a tap.
    Homebrew { formula: String },
    /// `cargo install` put it in a cargo bin directory.
    Cargo,
    /// A build tree, or somewhere nothing claims. Replacing it is the
    /// developer's business, so we refuse rather than guess.
    Unmanaged(PathBuf),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Homebrew { formula } => format!("Homebrew (formula {formula})"),
            Source::Cargo => "cargo install".into(),
            Source::Unmanaged(path) => format!("an unmanaged binary at {}", path.display()),
        }
    }

    /// The commands that replace this installation, in the order they must run.
    ///
    /// Both halves are always replaced together. The client refuses a daemon
    /// whose version differs from its own, so upgrading one of them would leave
    /// two pieces that will not talk to each other.
    pub fn upgrade(&self, force: bool) -> Result<Vec<Command>, String> {
        match self {
            Source::Homebrew { formula } => {
                let brew = tool("brew", "Homebrew installed this copy but `brew` is not on PATH")?;
                let mut command = Command::new(brew);
                // reinstall, not upgrade, when forced: brew treats an upgrade to
                // the version already installed as nothing to do.
                command.arg(if force { "reinstall" } else { "upgrade" });
                command.args(["--formula", formula]);
                Ok(vec![command])
            }
            Source::Cargo => {
                let cargo = tool("cargo", "cargo installed this copy but `cargo` is not on PATH")?;
                Ok(CRATES
                    .iter()
                    .map(|crate_name| {
                        let mut command = Command::new(&cargo);
                        // --force every time: cargo install refuses to overwrite
                        // a binary it already put there, which is the whole job.
                        command.args(["install", "--git", REPOSITORY, "--locked", "--force"]);
                        command.arg(crate_name);
                        command
                    })
                    .collect())
            }
            Source::Unmanaged(path) => Err(format!(
                "{} was not installed by Homebrew or cargo, so there is nothing to \
                 upgrade it with. From a checkout, `scripts/install-local.sh` builds \
                 that tree and installs both binaries over whatever your `xmusic` \
                 currently resolves to",
                path.display()
            )),
        }
    }
}

/// Which installer owns the running binary.
pub fn detect() -> Result<Source, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running xmusic binary: {error}"))?;
    // Homebrew links its binaries into its own bin, so the invoked path is a
    // symlink into the Cellar; resolve it or every install looks unmanaged.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    if let Some(formula) = homebrew_keg(&exe) {
        return Ok(Source::Homebrew { formula });
    }
    if in_cargo_bin(&exe) {
        return Ok(Source::Cargo);
    }
    Ok(Source::Unmanaged(exe))
}

/// The newest version published upstream, or `None` when nothing is.
///
/// Releases first, then tags. A release is what a project usually announces
/// with, but this one publishes by tag alone and the formula builds from a tag's
/// tarball, so a tag is a real version whether or not anybody wrote release
/// notes for it. Asking only about releases would report "nothing newer" forever.
pub fn latest_release() -> Result<Option<String>, String> {
    match published_release()? {
        Some(version) => Ok(Some(version)),
        None => newest_tag(),
    }
}

fn published_release() -> Result<Option<String>, String> {
    let release: serde_json::Value = match github(LATEST_RELEASE)? {
        Some(body) => body,
        None => return Ok(None),
    };
    let tag = release
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or("GitHub returned a release with no tag")?;
    Ok(Some(version_of(tag)))
}

/// The highest version among the tags, not the first one listed: GitHub returns
/// them in its own order, and that order is not version order.
fn newest_tag() -> Result<Option<String>, String> {
    let tags: serde_json::Value = match github(TAGS)? {
        Some(body) => body,
        None => return Ok(None),
    };
    let tags = tags.as_array().ok_or("GitHub returned unreadable tags")?;
    Ok(tags
        .iter()
        .filter_map(|tag| tag.get("name").and_then(serde_json::Value::as_str))
        .map(version_of)
        .filter_map(|version| triple(&version).map(|triple| (triple, version)))
        .max()
        .map(|(_, version)| version))
}

fn github(url: &str) -> Result<Option<serde_json::Value>, String> {
    let response = ureq::get(url)
        .timeout(CHECK_TIMEOUT)
        // GitHub rejects requests without one, and asks that it name the client.
        .set("User-Agent", concat!("xmusic/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .call();

    match response {
        Ok(body) => body
            .into_json()
            .map(Some)
            .map_err(|error| format!("GitHub returned unreadable JSON: {error}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(format!("GitHub answered HTTP {code}")),
        Err(ureq::Error::Transport(error)) => Err(error.to_string()),
    }
}

fn version_of(tag: &str) -> String {
    tag.trim_start_matches('v').to_string()
}

/// Whether `candidate` is a version worth moving to from `installed`.
///
/// Unparsable versions compare as plain strings: better to offer an upgrade that
/// turns out to be pointless than to hide one because a tag was named oddly.
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    match (triple(candidate), triple(installed)) {
        (Some(candidate), Some(installed)) => candidate > installed,
        _ => candidate != installed,
    }
}

/// A command written the way the user would have typed it, for the line that
/// says what is about to run.
pub fn spelled_out(command: &Command) -> String {
    let program = Path::new(command.get_program())
        .file_name()
        .unwrap_or(command.get_program())
        .to_string_lossy()
        .to_string();
    std::iter::once(program)
        .chain(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string()),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

fn triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let parsed = (next()?, next()?, next()?);
    parts.next().is_none().then_some(parsed)
}

/// `/opt/homebrew/Cellar/xmusic/0.3.0/bin/xmusic` names its own formula, so the
/// path is enough and no `brew` call is needed to identify the install.
fn homebrew_keg(exe: &Path) -> Option<String> {
    let mut components = exe.components().map(|component| component.as_os_str());
    components.find(|component| *component == "Cellar")?;
    Some(components.next()?.to_string_lossy().to_string())
}

fn in_cargo_bin(exe: &Path) -> bool {
    let parent = match exe.parent() {
        Some(parent) => parent,
        None => return false,
    };
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        roots.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cargo"));
    }
    roots.iter().any(|root| parent == root.join("bin"))
}

fn tool(name: &str, missing: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or_else(|| missing.to_string())?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| missing.to_string())
}
