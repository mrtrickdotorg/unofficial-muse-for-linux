//! Listing artwork for the rewrite: the feed's hero images, cached on
//! disk and rendered as pictures beside the card text. The Mac client
//! shows a hero image per unit; the service only gives this client the
//! URL, so the bytes are fetched once, capped, and reused from cache.
//!
//! Only the cache naming is unit-tested; the bytes need the network.

use std::path::PathBuf;

/// Cap for one artwork download: hero images are thumbnails, never
/// full attachments.
const MAX_BYTES: u64 = 5_000_000;

/// Stable cache name for a URL: FNV-1a hex (the std hasher is
/// deliberately unstable across runs, so it cannot name a cache).
fn cache_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    Some(base.join("muse-images"))
}

/// The cached file for a URL without touching the network: the first
/// paint reads from here so rows appear before any download lands.
pub fn fetch_hero_cached(url: &str) -> Option<PathBuf> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let target = cache_dir()?.join(cache_name(url));
    target.is_file().then_some(target)
}

/// How many hero downloads one feed fill chases: the first screenful.
/// The rest read text-only so a long roster never holds the paint.
pub const HERO_LANES: usize = 8;

/// Download heroes for the given (row index, URL) pairs. Order is
/// preserved and a failed download is `None`, never an error -- the
/// card renders text-only instead of failing the row.
pub fn fetch_heroes(requests: Vec<(usize, String)>) -> Vec<(usize, Option<PathBuf>)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = requests
            .into_iter()
            .map(|(index, url)| scope.spawn(move || (index, fetch_hero(&url))))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or((usize::MAX, None)))
            .collect()
    })
}
/// The cached file for a URL, downloading it first when missing.
/// Only http(s) URLs; anything else is not artwork. `None` on any
/// failure -- the card renders text-only instead of failing the row.
pub fn fetch_hero(url: &str) -> Option<PathBuf> {
    if let Some(hit) = fetch_hero_cached(url) {
        return Some(hit);
    }
    let dir = cache_dir()?;
    let target = dir.join(cache_name(url));
    std::fs::create_dir_all(&dir).ok()?;
    fetch_bounded(url, &target)
}

/// Download at most MAX_BYTES into `target`, failing cleanly above it.
fn fetch_bounded(url: &str, target: &PathBuf) -> Option<PathBuf> {
    let response = ureq::get(url)
        .set("User-Agent", "muse-linux-artwork")
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?;
    let mut reader = response.into_reader();
    let temporary = target.with_extension("part");
    let mut staged = std::fs::File::create(&temporary).ok()?;
    use std::io::Read;
    let mut remaining = MAX_BYTES;
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                use std::io::Write;
                let take = (n as u64).min(remaining) as usize;
                if staged.write_all(&buffer[..take]).is_err() {
                    let _ = std::fs::remove_file(&temporary);
                    return None;
                }
                remaining -= take as u64;
                if take < n {
                    let _ = std::fs::remove_file(&temporary);
                    let _ = std::fs::remove_file(target);
                    return None;
                }
            }
            Err(_) => {
                let _ = std::fs::remove_file(&temporary);
                return None;
            }
        }
    }
    std::fs::rename(&temporary, target).ok()?;
    Some(target.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names_are_stable_and_url_shaped() {
        assert_eq!(
            cache_name("https://example.invalid/a.png"),
            cache_name("https://example.invalid/a.png")
        );
        assert_ne!(
            cache_name("https://example.invalid/a.png"),
            cache_name("https://example.invalid/b.png")
        );
    }

    #[test]
    fn hero_batches_skip_non_artwork_without_network() {
        let got = fetch_heroes(vec![
            (0, "not a url".to_owned()),
            (2, "file:///etc/passwd".to_owned()),
        ]);
        assert_eq!(got, vec![(0, None), (2, None)]);
    }

    #[test]
    fn non_http_urls_are_never_artwork() {
        assert!(fetch_hero("file:///etc/passwd").is_none());
        assert!(fetch_hero("not a url").is_none());
    }
}
