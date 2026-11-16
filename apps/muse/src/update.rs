//! In-app updater for the rewrite: the Linux answer to the Mac
//! app's Sparkle feed (`SUFeedURL` appcast in the shipped bundle).
//!
//! Same contract, platform mechanism: read the rolling-release feed,
//! compare the built commit against the one compiled into this binary,
//! stage the new AppImage beside the running one, and relaunch.
//! Flatpak installs never reach this module -- the store owns those
//! updates, exactly like the App Store owns the Mac's.
//!
//! Only pure shapes are unit-tested (feed parsing, version compare,
//! check gating). The network runs on a worker thread and only the
//! outcome crosses back to the main thread.

use std::path::{Path, PathBuf};

/// The rolling-release feed: our equivalent of the Sparkle appcast.
/// The release notes name the built commit, which is the version.
pub const RELEASE_API: &str = "https://api.github.com/repos/mrtrickdotorg/unofficial-muse-for-linux/releases/tags/appimage-latest";
const ASSET_NAME: &str = "unofficial-muse-for-linux-x86_64.AppImage";
const CHECK_INTERVAL_SECS: u64 = 24 * 3600;

/// What the feed said: the commit it was built from plus where the
/// bits live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub sha: String,
    pub asset_url: String,
    pub asset_size: u64,
}

/// Pull the built commit out of the rolling-release notes
/// ("... built from <sha> by the AppImage workflow.").
pub fn parse_built_sha(notes: &str) -> Option<String> {
    let marker = "built from ";
    let start = notes.find(marker)? + marker.len();
    let token: String = notes[start..].chars().take(40).collect();
    if token.len() == 40 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token)
    } else {
        None
    }
}

/// Read the feed answer: notes for the version, assets for the bytes.
/// Unknown shapes are a client gap, never an update.
pub fn parse_release(json: &serde_json::Value) -> Result<ReleaseInfo, String> {
    let notes = json
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let sha =
        parse_built_sha(notes).ok_or_else(|| "Feed did not name a built commit.".to_string())?;
    let assets = json
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(serde_json::Value::as_str) == Some(ASSET_NAME))
        .ok_or_else(|| "Feed has no AppImage asset.".to_string())?;
    let asset_url = asset
        .get("browser_download_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Feed asset has no download URL.".to_string())?
        .to_owned();
    let asset_size = asset
        .get("size")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(ReleaseInfo {
        sha,
        asset_url,
        asset_size,
    })
}

/// An update exists when both sides name a commit and they differ.
/// A dev build (no compiled-in SHA) never counts as up to date and
/// never as behind: without an identity there is nothing to compare.
pub fn update_available(local_sha: Option<&str>, remote: &ReleaseInfo) -> bool {
    match local_sha {
        Some(local) if !local.is_empty() => local != remote.sha,
        _ => false,
    }
}

/// The commit this binary was built from, stamped by CI.
pub fn local_sha() -> Option<&'static str> {
    option_env!("UMFL_BUILD_SHA").filter(|sha| !sha.is_empty())
}

/// True when running from an AppImage bundle (the runtime sets
/// `APPDIR`). Anything else -- dev builds, Flatpak -- updates
/// through its own channel, never this module.
pub fn running_as_appimage() -> bool {
    std::env::var_os("APPDIR")
        .map(|dir| !dir.is_empty())
        .unwrap_or(false)
}

fn stamp_path() -> Option<PathBuf> {
    super::session::account_dir().map(|dir| dir.join("update-check.stamp"))
}

/// Whether a feed check is due: never checked, or the last check is
/// older than a day. A corrupt stamp reads as due, never as recent.
pub fn check_due(now_epoch: u64) -> bool {
    let path = match stamp_path() {
        Some(path) => path,
        None => return false,
    };
    let last = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    match last {
        Some(last) => now_epoch.saturating_sub(last) >= CHECK_INTERVAL_SECS,
        None => true,
    }
}

/// Record a completed check so the next one waits a day.
pub fn note_check_done(now_epoch: u64) {
    if let Some(path) = stamp_path() {
        let _ = std::fs::write(path, now_epoch.to_string());
    }
}

/// Fetch the feed answer. One blocking call, run on a worker.
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let response = ureq::get(RELEASE_API)
        .set("User-Agent", "muse-linux-updater")
        .set("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|error| format!("Update check failed: {error}"))?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|_| "Update feed returned invalid data.".to_string())?;
    parse_release(&json)
}

/// Where staged downloads land: a private folder under the cache home,
/// never beside the running binary (it may live somewhere read-only).
pub fn stage_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(base.join("muse-updates"))
}

/// Download the asset to the stage dir, atomically (temp file plus
/// rename) and executable. One blocking call, run on a worker.
pub fn download_staged(info: &ReleaseInfo) -> Result<PathBuf, String> {
    let dir = stage_dir().ok_or_else(|| "No cache home to stage the update into.".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|_| "Could not create the update stage.".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let target = dir.join(ASSET_NAME);
    let temporary = dir.join(format!(".{ASSET_NAME}.part"));
    let response = ureq::get(&info.asset_url)
        .set("User-Agent", "muse-linux-updater")
        .timeout(std::time::Duration::from_secs(600))
        .call()
        .map_err(|error| format!("Update download failed: {error}"))?;
    let mut staged = std::fs::File::create(&temporary)
        .map_err(|_| "Could not write the staged update.".to_string())?;
    let mut reader = response.into_reader();
    std::io::copy(&mut reader, &mut staged)
        .map_err(|_| "Update download was interrupted.".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| "Could not make the staged update executable.".to_string())?;
    }
    std::fs::rename(&temporary, &target)
        .map_err(|_| "Could not install the staged update.".to_string())?;
    Ok(target)
}

/// Relaunch from the staged file, verified: the new process must
/// still be alive a few seconds after spawn before this one exits.
/// Exiting into a failed launch is exactly the "go find the AppImage"
/// outcome this exists to prevent. The staged file stays in the cache
/// dir; the next update overwrites it. Blocking; run on a worker.
///
/// Marker the relauncher sets on the new copy only. It takes the
/// staged file as an independent instance instead of forwarding its
/// launch to this (still-running) one and exiting -- which is the
/// "app closed and never re-opened" outcome verbatim.
pub const UPDATING_ENV: &str = "MUSE_UPDATING";

pub fn updating_relaunch() -> bool {
    std::env::var_os(UPDATING_ENV).as_deref() == Some(std::ffi::OsStr::new("1"))
}

pub fn relaunch_verified(staged: &Path) -> Result<(), String> {
    let mut child = std::process::Command::new(staged)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // The new copy takes over: it must not forward to this one.
        .env(UPDATING_ENV, "1")
        // An AppImage child inherits the old mount's runtime paths;
        // the new version would load the old version's libraries and
        // die on startup. The runtime rebuilds these for itself.
        .env_remove("APPDIR")
        .env_remove("APPIMAGE")
        .env_remove("LD_LIBRARY_PATH")
        .spawn()
        .map_err(|_| {
            "Could not start the updated app. It is staged and executable — see the path below."
                .to_string()
        })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(Some(status)) => {
                return Err(format!(
                    "The updated app exited during startup ({status}). This copy keeps running; the staged file is below."
                ));
            }
            Err(_) => {
                return Err(
                    "Could not confirm the updated app started. This copy keeps running."
                        .to_string(),
                );
            }
        }
    }
}

/// The updates panel state, shared by the settings page builds.
/// Plain data: workers only write it from the main-thread idle
/// callback that applies their answer.
#[derive(Default)]
pub struct UpdateCheck {
    pub busy: bool,
    pub status: String,
    pub info: Option<ReleaseInfo>,
    pub staged: Option<PathBuf>,
}

pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(notes: &str) -> serde_json::Value {
        serde_json::json!({
            "body": notes,
            "assets": [
                {
                    "name": ASSET_NAME,
                    "browser_download_url": "https://example.invalid/muse.AppImage",
                    "size": 123,
                }
            ],
        })
    }

    #[test]
    fn built_sha_comes_from_the_notes_sentence() {
        let sha = "a".repeat(40);
        let notes = format!("Rolling x86_64 AppImage built from {sha} by the AppImage workflow.");
        assert_eq!(parse_built_sha(&notes), Some(sha));
        assert_eq!(parse_built_sha("no marker here"), None);
        assert_eq!(parse_built_sha("built from short"), None);
    }

    #[test]
    fn feed_answer_parses_into_release_facts() {
        let sha = "b".repeat(40);
        let info = parse_release(&feed(&format!("built from {sha}."))).expect("parses");
        assert_eq!(info.sha, sha);
        assert_eq!(info.asset_url, "https://example.invalid/muse.AppImage");
        assert_eq!(info.asset_size, 123);
        assert!(parse_release(&feed("no sha")).is_err());
        assert!(
            parse_release(
                &serde_json::json!({"body": "built from cccccccccccccccccccccccccccccccccccccccc."})
            )
            .is_err()
        );
    }

    #[test]
    fn only_a_different_named_commit_counts_as_an_update() {
        let remote = ReleaseInfo {
            sha: "a".repeat(40),
            asset_url: String::new(),
            asset_size: 0,
        };
        assert!(!update_available(Some(&"a".repeat(40)), &remote));
        assert!(update_available(Some(&"b".repeat(40)), &remote));
        assert!(!update_available(None, &remote));
        assert!(!update_available(Some(""), &remote));
    }

    #[test]
    fn local_sha_reports_exactly_what_was_compiled_in() {
        // CI stamps UMFL_BUILD_SHA; dev checkouts don't. Either way the
        // accessor must report the compiled-in value verbatim, so the
        // updater compares against the real identity.
        let compiled: Option<&str> = option_env!("UMFL_BUILD_SHA");
        assert_eq!(local_sha(), compiled.filter(|sha| !sha.is_empty()));
    }
}
