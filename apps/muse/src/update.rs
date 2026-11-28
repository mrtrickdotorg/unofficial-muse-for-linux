//! In-app updater for the rewrite: the Linux answer to the Mac
//! app's Sparkle feed (`SUFeedURL` appcast in the shipped bundle).
//!
//! Same contract, platform mechanism: read the rolling-release feed,
//! compare the built commit against the one compiled into this binary,
//! stage the new AppImage beside the running one, relaunch from the
//! stage once the new copy proves it stays up, then refresh the
//! launched file in place so the next launcher start opens the new
//! build. Flatpak installs never reach this module -- the store owns
//! those updates, exactly like the App Store owns the Mac's.
//!
//! A staged download is verified against the feed before it can be
//! run: the byte count must match the size the feed published and
//! the SHA-256 must match the digest the feed published. A feed
//! without a digest is refused, never taken on trust -- the staged
//! file is executed, so an unverified download is remote code.
//!
//! Builds are ordered by when they were built, not merely by which
//! commit they name: every build is stamped with its build instant
//! and the feed publishes the same instant beside its commit, and
//! an offered build older than the running one is refused. A
//! different commit is not on its own an update -- a stale or
//! replayed feed must not walk the app backwards.
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
    /// The SHA-256 the feed published for the asset, as lowercase
    /// hex without the `sha256:` prefix. `None` when the feed
    /// published no digest; a download is refused in that case.
    pub asset_digest: Option<String>,
    /// When the offered build was built (seconds since the epoch),
    /// as published in the feed's notes beside its commit. `None`
    /// when the notes name no instant; such a build cannot be
    /// proven not older than the running one and is not offered.
    pub build_epoch: Option<u64>,
}

/// The identifier the panel names for an offered build: the shortened
/// commit the feed says the release was built from.
pub fn short_build(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// The asset's bytes as megabytes ("82.1 MB"), or `None` when the feed
/// published no size for the asset.
pub fn asset_size_mb(bytes: u64) -> Option<String> {
    if bytes == 0 {
        return None;
    }
    Some(format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)))
}

/// The offered card's sentence: what the feed holds, not merely that it
/// holds something. A feed that published no size says so instead of
/// quietly leaving the figure out.
pub fn offered_status(info: &ReleaseInfo) -> String {
    let build = short_build(&info.sha);
    match asset_size_mb(info.asset_size) {
        Some(size) => format!("A newer build is available: {build} ({size})."),
        None => format!("A newer build is available: {build} (the feed reports no size)."),
    }
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

/// Pull the build instant out of the rolling-release notes
/// ("... Build epoch 1764300000."). Notes without one, or with an
/// instant that does not parse, name no instant.
pub fn parse_built_epoch(notes: &str) -> Option<u64> {
    let marker = "Build epoch ";
    let start = notes.find(marker)? + marker.len();
    let digits: String = notes[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok().filter(|epoch| *epoch > 0)
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
    let build_epoch = parse_built_epoch(notes);
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
    let asset_digest = parse_asset_digest(asset)?;
    Ok(ReleaseInfo {
        sha,
        asset_url,
        asset_size,
        asset_digest,
        build_epoch,
    })
}

/// The digest the feed published for the asset, normalized to
/// lowercase hex. The feed's form is `sha256:<64 hex digits>`. An
/// absent digest is `None` (the download refuses it later); a
/// present one in any other shape is a feed this client cannot
/// verify against, which is a parse failure, never a quiet pass --
/// a checksum that cannot be read must not become a download that
/// is not checked.
fn parse_asset_digest(asset: &serde_json::Value) -> Result<Option<String>, String> {
    let Some(raw) = asset.get("digest") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let raw = raw
        .as_str()
        .ok_or_else(|| "Feed asset digest is unreadable.".to_string())?;
    let hex = raw
        .strip_prefix("sha256:")
        .ok_or_else(|| "Feed asset digest is unreadable.".to_string())?;
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(hex.to_lowercase()))
    } else {
        Err("Feed asset digest is unreadable.".to_string())
    }
}

/// An update exists when the feed names a different commit that
/// was built no earlier than this one. A dev build (no compiled-in
/// SHA) never counts as up to date and never as behind: without an
/// identity there is nothing to compare. A feed naming no build
/// instant proves nothing about its age, so it is not an update
/// either; a local build with no stamped instant (one from before
/// the stamp existed) counts as the oldest there is, so its way
/// onto the stamped feed stays open. Two builds stamped in the
/// same instant cannot be ordered and the different commit stands.
pub fn update_available(
    local_sha: Option<&str>,
    local_epoch: Option<u64>,
    remote: &ReleaseInfo,
) -> bool {
    let Some(local) = local_sha.filter(|sha| !sha.is_empty()) else {
        return false;
    };
    if local == remote.sha {
        return false;
    }
    let Some(remote_epoch) = remote.build_epoch else {
        return false;
    };
    remote_epoch >= local_epoch.unwrap_or(0)
}

/// The commit this binary was built from, stamped by CI.
pub fn local_sha() -> Option<&'static str> {
    option_env!("UMFL_BUILD_SHA").filter(|sha| !sha.is_empty())
}

/// When this binary was built (seconds since the epoch), stamped
/// beside the commit by the same build that stamped it.
pub fn local_epoch() -> Option<u64> {
    option_env!("UMFL_BUILD_EPOCH")
        .and_then(|text| text.parse().ok())
        .filter(|epoch| *epoch > 0)
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

/// A release feed on disk, set by the two-window harness
/// (`scripts/settings-two-windows.py`) so a seat can be driven to the offered
/// and staged states without the network or a real build. A shipping AppImage
/// never sets it, and every use below is behind it.
pub fn harness_feed() -> Option<PathBuf> {
    std::env::var_os("MUSE_UPDATE_FEED")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Whether a harness copy may take its Restart press for real. The two-window
/// rounds refuse it by default, so they cannot replace the build under them;
/// the script's `--restart` round instead runs a throwaway copy and says so
/// here, which is the only way the relaunch itself is ever driven.
pub fn restart_allowed() -> bool {
    std::env::var_os("MUSE_UPDATE_RESTART").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// Pace the harness's own work so the busy step lasts long enough for the
/// script to read both cards inside it. Zero (and no sleep) unless a feed is
/// set, so a shipping copy never pays for it.
fn harness_delay() {
    if harness_feed().is_none() {
        return;
    }
    let millis = std::env::var("MUSE_UPDATE_DELAY_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if millis > 0 {
        std::thread::sleep(std::time::Duration::from_millis(millis));
    }
}

fn read_feed(path: &Path) -> Result<ReleaseInfo, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("Update check failed: {error}"))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| "Update feed returned invalid data.".to_string())?;
    parse_release(&json)
}

/// A `file://` asset the harness points the feed at: the same bytes a download
/// would fetch, without the network round trip. Only the absolute form counts;
/// anything else takes the normal download path.
fn local_asset(url: &str) -> Option<PathBuf> {
    let path = PathBuf::from(url.strip_prefix("file://")?);
    path.is_absolute().then_some(path)
}

/// The agent both fetches share: a TCP handshake that never completes is the
/// one wait a request timeout does not see, so connecting is bounded separately
/// and the card reports a failure instead of holding its busy step.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .build()
}

/// Fetch the feed answer. One blocking call, run on a worker.
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    if let Some(path) = harness_feed() {
        harness_delay();
        return read_feed(&path);
    }
    let response = agent()
        .get(RELEASE_API)
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

/// The SHA-256 of a file's bytes, as lowercase hex, streamed in
/// chunks so a full build never sits in memory twice. GLib's
/// checksum does the hashing: it is the platform's maintained
/// implementation and already linked into the app through GTK, so
/// the updater's verification needs no new crate of its own.
fn sha256_hex(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).map_err(|_| "Could not read the staged update.".to_string())?;
    let mut checksum = glib::Checksum::new(glib::ChecksumType::Sha256)
        .ok_or_else(|| "Could not read the staged update.".to_string())?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "Could not read the staged update.".to_string())?;
        if read == 0 {
            break;
        }
        checksum.update(&buffer[..read]);
    }
    checksum
        .string()
        .ok_or_else(|| "Could not read the staged update.".to_string())
}

/// Prove a downloaded file is the build the feed described before
/// it is made executable or installed: the feed must have published
/// a digest at all, the byte count must match the published size
/// when one was published, and the SHA-256 must match the digest.
/// Any doubt refuses -- the staged file is executed, so a download
/// that cannot be verified is not an update.
pub fn verify_download(path: &Path, info: &ReleaseInfo) -> Result<(), String> {
    let Some(expected) = &info.asset_digest else {
        return Err(
            "The feed published no checksum for this build, so the download cannot be verified. Update refused."
                .to_string(),
        );
    };
    if info.asset_size > 0 {
        let len = std::fs::metadata(path)
            .map_err(|_| "Could not read the staged update.".to_string())?
            .len();
        if len != info.asset_size {
            return Err(
                "The downloaded build is not the size the feed published. Update refused."
                    .to_string(),
            );
        }
    }
    if sha256_hex(path)? != *expected {
        return Err(
            "The downloaded build does not match the checksum the feed published. Update refused."
                .to_string(),
        );
    }
    Ok(())
}

/// Download the asset to the stage dir, atomically (temp file plus
/// rename) and executable. The temp file is verified against the
/// feed (see `verify_download`) before the rename installs it, so
/// the staged path only ever holds a build that was proven. One
/// blocking call, run on a worker.
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
    if let (Some(_), Some(source)) = (harness_feed(), local_asset(&info.asset_url)) {
        harness_delay();
        let bytes =
            std::fs::read(&source).map_err(|error| format!("Update download failed: {error}"))?;
        std::fs::write(&temporary, &bytes)
            .map_err(|_| "Could not write the staged update.".to_string())?;
    } else {
        let response = agent()
            .get(&info.asset_url)
            .set("User-Agent", "muse-linux-updater")
            .timeout(std::time::Duration::from_secs(600))
            .call()
            .map_err(|error| format!("Update download failed: {error}"))?;
        let mut staged = std::fs::File::create(&temporary)
            .map_err(|_| "Could not write the staged update.".to_string())?;
        let mut reader = response.into_reader();
        std::io::copy(&mut reader, &mut staged)
            .map_err(|_| "Update download was interrupted.".to_string())?;
    }
    if let Err(error) = verify_download(&temporary, info) {
        // A refused download leaves nothing behind: the temp file
        // is not executable and never renamed into place, and it
        // goes away here so the stage holds no unproven build.
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
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

/// Copy the verified staged update over the launched file so the
/// launcher entry (Gearlever, a .desktop, or a stray copy) opens the
/// new build next time. Same-directory temp file plus rename: the
/// running mount keeps its own inode while new launches see the new
/// bytes. Pure filesystem work, covered below with temp files.
pub fn refresh_launched_file(appimage: &Path, staged: &Path) -> Result<(), String> {
    let tmp = appimage.with_extension("updating");
    std::fs::copy(staged, &tmp)
        .map_err(|_| "Could not write the refreshed launcher copy.".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| "Could not make the refreshed launcher copy executable.".to_string())?;
    }
    std::fs::rename(&tmp, appimage)
        .map_err(|_| "Could not install the refreshed launcher copy.".to_string())?;
    Ok(())
}

/// Refresh the launched file when this process is an AppImage: the
/// running copy knows its own path through `APPIMAGE`. Returns the
/// refreshed path, or `None` outside a bundle (a checkout or system
/// install has no launcher copy to keep current).
pub fn refresh_launcher_copy(staged: &Path) -> Result<Option<PathBuf>, String> {
    let Some(appimage) = std::env::var_os("APPIMAGE") else {
        return Ok(None);
    };
    let appimage = PathBuf::from(appimage);
    refresh_launched_file(&appimage, staged)?;
    Ok(Some(appimage))
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

    #[test]
    fn launcher_refresh_replaces_in_place_and_stays_executable() {
        let dir = std::env::temp_dir().join(format!("muse-refresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe dir");
        let launched = dir.join("muse-old.AppImage");
        let staged = dir.join("staged.AppImage");
        std::fs::write(&launched, b"old").expect("old copy");
        std::fs::write(&staged, b"new").expect("staged copy");
        refresh_launched_file(&launched, &staged).expect("refresh");
        assert_eq!(std::fs::read(&launched).expect("read"), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&launched)
                .expect("meta")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0);
        }
        assert!(!dir.join("muse-old.updating").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_offered_build_is_named_with_its_bytes() {
        let info = ReleaseInfo {
            sha: "a6ed625ca4f8cabad4c2bb3cefcaebfdbec23b4e".to_owned(),
            asset_url: "file:///tmp/muse.AppImage".to_owned(),
            asset_size: 86_071_800,
            asset_digest: None,
            build_epoch: None,
        };
        assert_eq!(
            offered_status(&info),
            "A newer build is available: a6ed625 (82.1 MB)."
        );
    }

    #[test]
    fn a_feed_without_an_asset_size_says_so() {
        let info = ReleaseInfo {
            sha: "0f1e2d3c4b5a69788796a5b4c3d2e1f0a1b2c3d4".to_owned(),
            asset_url: String::new(),
            asset_size: 0,
            asset_digest: None,
            build_epoch: None,
        };
        assert_eq!(
            offered_status(&info),
            "A newer build is available: 0f1e2d3 (the feed reports no size)."
        );
    }

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
    fn feed_digest_parses_normalized_and_missing_is_none() {
        let sha = "e".repeat(40);
        let notes = format!("built from {sha}.");
        let mut with = feed(&notes);
        with["assets"][0]["digest"] = serde_json::json!(format!("sha256:{}", "AB".repeat(32)));
        let info = parse_release(&with).expect("parses");
        assert_eq!(info.asset_digest, Some("ab".repeat(32)));
        // No digest field, and an explicit null, both read as none.
        let info = parse_release(&feed(&notes)).expect("parses");
        assert_eq!(info.asset_digest, None);
        let mut null = feed(&notes);
        null["assets"][0]["digest"] = serde_json::Value::Null;
        let info = parse_release(&null).expect("parses");
        assert_eq!(info.asset_digest, None);
    }

    #[test]
    fn a_malformed_feed_digest_is_a_parse_failure() {
        let sha = "e".repeat(40);
        let notes = format!("built from {sha}.");
        for bad in [
            serde_json::json!("sha256:xyz"),
            serde_json::json!(format!("sha256:{}", "ab".repeat(31))),
            serde_json::json!(format!("md5:{}", "ab".repeat(32))),
            serde_json::json!(123),
        ] {
            let mut value = feed(&notes);
            value["assets"][0]["digest"] = bad;
            assert!(parse_release(&value).is_err(), "{value}");
        }
    }

    /// sha256("abc"), the standard test vector: verification is
    /// checked against a digest known outside this code, not one
    /// the code computed itself.
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn staged_file(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("staged.AppImage");
        std::fs::write(&path, bytes).expect("staged file");
        path
    }

    fn info_with(size: u64, digest: Option<&str>) -> ReleaseInfo {
        ReleaseInfo {
            sha: "f".repeat(40),
            asset_url: String::new(),
            asset_size: size,
            asset_digest: digest.map(str::to_owned),
            build_epoch: None,
        }
    }

    #[test]
    fn a_download_is_verified_against_the_feed_before_it_can_run() {
        let dir = std::env::temp_dir().join(format!("muse-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe dir");
        let path = staged_file(&dir, b"abc");
        // The build the feed described: passes.
        assert!(verify_download(&path, &info_with(3, Some(ABC_SHA256))).is_ok());
        // A size the feed did not publish skips only the size check.
        assert!(verify_download(&path, &info_with(0, Some(ABC_SHA256))).is_ok());
        // A different size than the feed published: refused.
        assert!(verify_download(&path, &info_with(4, Some(ABC_SHA256))).is_err());
        // A different build than the feed's checksum (sha256 of the
        // empty string): refused.
        assert!(
            verify_download(
                &path,
                &info_with(
                    3,
                    Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                )
            )
            .is_err()
        );
        // No checksum in the feed at all: refused, never on trust.
        assert!(verify_download(&path, &info_with(3, None)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The harness feed is read exactly like a network answer, and an asset
    /// URL only becomes a local copy for the absolute `file://` form.
    #[test]
    fn a_harness_feed_reads_like_the_network_answer() {
        let dir = std::env::temp_dir().join(format!("muse-feed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe dir");
        let sha = "d".repeat(40);
        let path = dir.join("feed.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "body": format!("Rolling x86_64 AppImage built from {sha} by the AppImage workflow."),
                "assets": [{
                    "name": ASSET_NAME,
                    "browser_download_url": "file:///tmp/muse-harness.AppImage",
                    "size": 42,
                }],
            })
            .to_string(),
        )
        .expect("feed file");
        let info = read_feed(&path).expect("reads the file");
        assert_eq!(info.sha, sha);
        assert_eq!(info.asset_url, "file:///tmp/muse-harness.AppImage");
        assert_eq!(info.asset_size, 42);
        assert!(read_feed(&dir.join("missing.json")).is_err());
        assert_eq!(
            local_asset("file:///tmp/muse-harness.AppImage"),
            Some(PathBuf::from("/tmp/muse-harness.AppImage"))
        );
        assert_eq!(local_asset("https://example.invalid/muse.AppImage"), None);
        assert_eq!(local_asset("file://relative.AppImage"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn remote_at(epoch: Option<u64>) -> ReleaseInfo {
        ReleaseInfo {
            sha: "a".repeat(40),
            asset_url: String::new(),
            asset_size: 0,
            asset_digest: None,
            build_epoch: epoch,
        }
    }

    #[test]
    fn only_a_different_commit_built_no_earlier_counts_as_an_update() {
        let local = "b".repeat(40);
        // The same commit is never an update, whatever its instant.
        assert!(!update_available(
            Some(&"a".repeat(40)),
            Some(100),
            &remote_at(Some(200))
        ));
        // A different commit built later: an update.
        assert!(update_available(
            Some(&local),
            Some(100),
            &remote_at(Some(200))
        ));
        // The same instant cannot be ordered; the different commit stands.
        assert!(update_available(
            Some(&local),
            Some(100),
            &remote_at(Some(100))
        ));
        // A different commit built earlier: refused -- a stale or
        // replayed feed does not walk the app backwards.
        assert!(!update_available(
            Some(&local),
            Some(200),
            &remote_at(Some(100))
        ));
        // A feed naming no instant proves nothing about its age.
        assert!(!update_available(Some(&local), Some(100), &remote_at(None)));
        // A local build with no stamped instant counts as the oldest
        // there is, so its way onto the stamped feed stays open.
        assert!(update_available(Some(&local), None, &remote_at(Some(100))));
        // No local identity: nothing to compare, never an update.
        assert!(!update_available(None, None, &remote_at(Some(200))));
        assert!(!update_available(Some(""), None, &remote_at(Some(200))));
    }

    #[test]
    fn built_epoch_comes_from_the_notes_sentence() {
        let notes = "Rolling x86_64 AppImage built from abc by the AppImage workflow. Build epoch 1764300000.";
        assert_eq!(parse_built_epoch(notes), Some(1_764_300_000));
        assert_eq!(parse_built_epoch("no marker here"), None);
        assert_eq!(parse_built_epoch("Build epoch soon."), None);
        assert_eq!(parse_built_epoch("Build epoch 0."), None);
    }

    #[test]
    fn local_epoch_reports_exactly_what_was_compiled_in() {
        // Same discipline as the commit stamp: the accessor reports
        // the compiled-in instant verbatim, so the ordering compares
        // the real build moment.
        let compiled: Option<u64> = option_env!("UMFL_BUILD_EPOCH")
            .and_then(|text| text.parse().ok())
            .filter(|epoch| *epoch > 0);
        assert_eq!(local_epoch(), compiled);
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
