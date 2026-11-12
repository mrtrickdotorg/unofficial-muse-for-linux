//! In-app updates for the AppImage build.
//!
//! The old flow — open the releases page, download by hand, find the
//! file, run it — was the standing friction Patrick named on Nov 11:
//! "a way to update the app without having to constantly download and
//! run and re-authenticate." This module is the whole mechanism:
//!
//! 1. Ask GitHub's API about the rolling `appimage-latest` release.
//! 2. Compare it with the running build (by commit when CI recorded
//!    one in the binary, else by build time).
//! 3. Download the asset, verifying its size and the sha256 digest
//!    GitHub publishes for it, to a `.part` sibling of the running
//!    AppImage file.
//! 4. Atomically rename the verified file over the running one — the
//!    path never changes, so launchers, permissions and muscle memory
//!    all keep working — and let the caller offer a restart.
//!
//! Only the binary file is ever replaced. The account session lives in
//! ~/.config/unofficial-muse-for-linux and is untouched, so an update
//! never asks for sign-in again. Nothing here runs unless the app is
//! actually running from an AppImage (the APPIMAGE environment
//! variable the AppImage runtime sets); a source build keeps the old
//! open-the-releases-page behaviour.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::Digest;

pub const RELEASE_API_URL: &str = "https://api.github.com/repos/mrtrickdotorg/unofficial-muse-for-linux/releases/tags/appimage-latest";
pub const RELEASES_PAGE: &str =
    "https://github.com/mrtrickdotorg/unofficial-muse-for-linux/releases";
pub const ASSET_NAME: &str = "unofficial-muse-for-linux-x86_64.AppImage";
/// How long the launch-time check waits before looking again.
pub const CHECK_TTL_SECONDS: u64 = 12 * 60 * 60;

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseInfo {
    /// The commit the release was built from, read out of the release
    /// notes the workflow writes ("built from <sha>").
    pub sha: Option<String>,
    pub download_url: String,
    pub size: u64,
    /// Lowercase hex sha256 from the asset's `digest` field, when the
    /// API provides one.
    pub sha256: Option<String>,
    /// When the asset was uploaded, seconds since the Unix epoch.
    pub updated_epoch: u64,
}

/// Parse the GitHub release JSON into the facts the updater needs.
/// Pure, so the shape is fixture-tested against the real API payload.
pub fn parse_release(json: &serde_json::Value) -> Result<ReleaseInfo, String> {
    let body = json
        .get("body")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let sha = extract_built_sha(body);
    let assets = json
        .get("assets")
        .and_then(|value| value.as_array())
        .ok_or("the release answer carried no asset list")?;
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(|value| value.as_str()) == Some(ASSET_NAME))
        .ok_or("the release carries no AppImage asset")?;
    let download_url = asset
        .get("browser_download_url")
        .and_then(|value| value.as_str())
        .filter(|url| url.starts_with("https://"))
        .ok_or("the AppImage asset has no download address")?
        .to_owned();
    let size = asset
        .get("size")
        .and_then(|value| value.as_u64())
        .filter(|size| *size > 0)
        .ok_or("the AppImage asset reports no size")?;
    let sha256 = asset
        .get("digest")
        .and_then(|value| value.as_str())
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(|hex| hex.to_ascii_lowercase());
    let updated_epoch = asset
        .get("updated_at")
        .and_then(|value| value.as_str())
        .and_then(parse_iso_epoch)
        .ok_or("the AppImage asset reports no upload time")?;
    Ok(ReleaseInfo {
        sha,
        download_url,
        size,
        sha256,
        updated_epoch,
    })
}

/// The workflow's notes say "built from <sha>"; pull the hex run out.
fn extract_built_sha(body: &str) -> Option<String> {
    let start = body.find("built from ")?.checked_add("built from ".len())?;
    let run: String = body[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect();
    if (7..=64).contains(&run.len()) {
        Some(run.to_ascii_lowercase())
    } else {
        None
    }
}

/// True when the release is newer than the running build. Commit
/// comparison is exact when both sides recorded one; otherwise build
/// times decide, with five minutes of slack so a release uploaded
/// moments after its build is not mistaken for news by a binary whose
/// clock is its own file time.
pub fn update_available(
    current_sha: Option<&str>,
    current_built_epoch: Option<u64>,
    release: &ReleaseInfo,
) -> bool {
    if let (Some(current), Some(latest)) = (current_sha, release.sha.as_deref()) {
        let current = current.to_ascii_lowercase();
        return current != latest && !latest.starts_with(&current) && !current.starts_with(latest);
    }
    match current_built_epoch {
        Some(built) => release.updated_epoch > built + 300,
        None => false,
    }
}

/// Strict `YYYY-MM-DDTHH:MM:SSZ` to epoch seconds (the only shape the
/// GitHub API emits for asset timestamps).
pub fn parse_iso_epoch(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |from: usize, len: usize| -> Option<i64> { text[from..from + len].parse().ok() };
    let days = days_from_civil(field(0, 4)?, field(5, 2)? as u32, field(8, 2)? as u32);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(field(11, 2)? * 3_600 + field(14, 2)? * 60 + field(17, 2)?)?;
    u64::try_from(seconds).ok()
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm,
/// the inverse of build_identity's conversion).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The running AppImage's own file, from the environment the AppImage
/// runtime sets. None for a source build or an unpacked tree — those
/// keep the releases-page behaviour.
pub fn appimage_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    if path.is_absolute() && path.is_file() {
        Some(path)
    } else {
        None
    }
}

fn stamp_path() -> Option<PathBuf> {
    Some(
        gtk4::glib::user_config_dir()
            .join("unofficial-muse-for-linux")
            .join("update-check"),
    )
}

/// True when the launch-time check should run: no stamp, an unreadable
/// stamp, or a stamp older than the TTL.
pub fn check_due(now_epoch: u64) -> bool {
    let Some(path) = stamp_path() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    match text.trim().parse::<u64>() {
        Ok(stamped) => now_epoch.saturating_sub(stamped) >= CHECK_TTL_SECONDS,
        Err(_) => true,
    }
}

pub fn note_check_done(now_epoch: u64) {
    if let Some(path) = stamp_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, now_epoch.to_string());
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!(
            "unofficial-muse-for-linux/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|_| "could not start the update check".to_string())
}

/// Ask GitHub about the rolling release.
pub fn fetch_latest() -> Result<ReleaseInfo, String> {
    let response = client()?
        .get(RELEASE_API_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| "could not reach GitHub to check for updates".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub answered {} to the update check",
            response.status().as_u16()
        ));
    }
    let json: serde_json::Value = response
        .json()
        .map_err(|_| "GitHub's update answer could not be read".to_string())?;
    parse_release(&json)
}

/// Download the release over `target` (the running AppImage's path),
/// verifying the byte count and, when GitHub published one, the sha256
/// digest before anything is installed. The verified file replaces the
/// target by atomic rename; any failure removes the partial download
/// and leaves the running build exactly as it was.
pub fn download_and_install(
    release: &ReleaseInfo,
    target: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let mut response = client()?
        .get(&release.download_url)
        .send()
        .map_err(|_| "could not start the update download".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "the update download was answered {}",
            response.status().as_u16()
        ));
    }
    let mut part_os = target.as_os_str().to_owned();
    part_os.push(".part");
    let part = PathBuf::from(part_os);
    let outcome = (|| {
        let mut file = std::fs::File::create(&part)
            .map_err(|_| "could not create the update file next to the app".to_string())?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut done: u64 = 0;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|_| "the update download was interrupted".to_string())?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|_| "could not write the update file".to_string())?;
            hasher.update(&buffer[..read]);
            done += read as u64;
            progress(done, release.size);
        }
        file.sync_all()
            .map_err(|_| "could not finish the update file".to_string())?;
        if done != release.size {
            return Err(format!(
                "the update download was incomplete ({done} of {} bytes)",
                release.size
            ));
        }
        if let Some(expected) = &release.sha256 {
            let actual = hex_lower(&hasher.finalize());
            if actual != *expected {
                return Err(
                    "the downloaded update does not match the checksum GitHub published; nothing was installed"
                        .to_string(),
                );
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))
                .map_err(|_| "could not make the update executable".to_string())?;
        }
        std::fs::rename(&part, target)
            .map_err(|_| "could not install the update over the old build".to_string())?;
        if let Some(dir) = target.parent()
            && let Ok(dir_file) = std::fs::File::open(dir)
        {
            let _ = dir_file.sync_all();
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    outcome
}

/// Start the freshly installed AppImage as a new, detached process.
/// The AppImage runtime variables of the current process are stripped
/// so the new runtime sets its own; the caller exits afterwards.
pub fn relaunch(path: &Path) -> Result<(), String> {
    std::process::Command::new(path)
        .env_remove("APPDIR")
        .env_remove("APPIMAGE")
        .env_remove("ARGV0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "the updated app could not be started".to_string())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> serde_json::Value {
        serde_json::json!({
            "tag_name": "appimage-latest",
            "name": "Latest AppImage (built from main)",
            "body": "Rolling x86_64 AppImage built from a4618b17bee633c99ce04dbab092ab44dca98cfc by the AppImage workflow.",
            "assets": [
                {
                    "name": "checksums.txt",
                    "browser_download_url": "https://example.invalid/checksums.txt",
                    "size": 128,
                    "digest": null,
                    "updated_at": "2026-11-12T00:46:11Z"
                },
                {
                    "name": ASSET_NAME,
                    "browser_download_url": "https://github.com/mrtrickdotorg/unofficial-muse-for-linux/releases/download/appimage-latest/unofficial-muse-for-linux-x86_64.AppImage",
                    "size": 48_000_000,
                    "digest": "sha256:ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789",
                    "updated_at": "2026-11-12T00:46:11Z"
                }
            ]
        })
    }

    #[test]
    fn a_release_answer_parses_into_updater_facts() {
        let release = parse_release(&fixture()).unwrap();
        assert_eq!(
            release.sha.as_deref(),
            Some("a4618b17bee633c99ce04dbab092ab44dca98cfc")
        );
        assert_eq!(release.size, 48_000_000);
        assert_eq!(
            release.sha256.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(release.updated_epoch, 1_794_444_371);
        assert!(release.download_url.ends_with(ASSET_NAME));
    }

    #[test]
    fn a_release_without_the_appimage_asset_is_an_error() {
        let mut json = fixture();
        json["assets"] = serde_json::json!([]);
        assert!(parse_release(&json).is_err());
    }

    #[test]
    fn iso_timestamps_parse_and_garbage_does_not() {
        assert_eq!(parse_iso_epoch("2026-01-01T00:00:00Z"), Some(1_767_225_600));
        assert_eq!(parse_iso_epoch("2026-11-12T00:46:11Z"), Some(1_794_444_371));
        assert_eq!(parse_iso_epoch("2026-11-12 00:46:11"), None);
        assert_eq!(parse_iso_epoch("not a date"), None);
        assert_eq!(parse_iso_epoch(""), None);
    }

    #[test]
    fn availability_prefers_commits_and_falls_back_to_times() {
        let mut release = parse_release(&fixture()).unwrap();
        // Same commit: no update, however the times look.
        assert!(!update_available(
            Some("a4618b17bee633c99ce04dbab092ab44dca98cfc"),
            Some(0),
            &release
        ));
        // A short recorded SHA is the same commit, not a new one.
        assert!(!update_available(Some("a4618b1"), Some(0), &release));
        // A different commit: update.
        assert!(update_available(
            Some("c98d07670988b86a7ea000fae72f1919fbf16bd5"),
            Some(0),
            &release
        ));
        // No commit on either side: the upload must be clearly newer
        // than the running build's own time.
        release.sha = None;
        assert!(update_available(
            None,
            Some(1_794_444_371 - 3_600),
            &release
        ));
        assert!(!update_available(None, Some(1_794_444_371 - 60), &release));
        assert!(!update_available(
            None,
            Some(1_794_444_371 + 3_600),
            &release
        ));
        // Nothing known about the running build: never claim an update.
        assert!(!update_available(None, None, &release));
    }
}
