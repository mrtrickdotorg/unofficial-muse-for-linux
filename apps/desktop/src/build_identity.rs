//! Build identity for the window's lower-left corner.
//!
//! An AppImage sitting in a Downloads folder is otherwise indistinguishable from
//! yesterday's build, which is exactly how a fixed bug gets reported again. The
//! running binary's own timestamp answers "which build is this": the AppImage
//! preserves each bundled file's build time inside the squashfs image.

/// `v0.1.0 · build 2026-11-03 19:43 UTC`
pub fn label() -> String {
    match stamp() {
        Some(stamp) => format!("v{} · build {stamp}", env!("CARGO_PKG_VERSION")),
        None => format!("v{}", env!("CARGO_PKG_VERSION")),
    }
}

fn stamp() -> Option<String> {
    let modified = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok())?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(utc_stamp(seconds))
}

pub fn utc_stamp(seconds: u64) -> String {
    let (year, month, day) = civil_date((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        (rest % 3600) / 60
    )
}

/// Civil date for days since 1970-01-01 (Howard Hinnant's algorithm), so the
/// label needs no date crate for six fields of formatting.
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_names_a_real_utc_instant() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(86_399), "1970-01-01 23:59 UTC");
        assert_eq!(utc_stamp(86_400), "1970-01-02 00:00 UTC");
        // 2001-09-09T01:46:40Z, the epoch second that is easy to mistrust.
        assert_eq!(utc_stamp(1_000_000_000), "2001-09-09 01:46 UTC");
        // A leap day, so the month arithmetic is exercised off the easy path.
        assert_eq!(utc_stamp(1_709_164_800), "2024-02-29 00:00 UTC");
    }

    #[test]
    fn the_label_always_names_the_version() {
        assert!(label().starts_with(concat!("v", env!("CARGO_PKG_VERSION"))));
    }
}
