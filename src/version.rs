//! App version stamping for profiles.
//!
//! A profile records the newest Slidr that has written it. Opening a profile
//! from a newer build with an older app is the dangerous direction: the load
//! itself is harmless (serde ignores fields it doesn't know), but the next save
//! writes the profile back *without* them — silently dropping whatever the newer
//! version stored. So the stamp is compared on load, and the marker is only ever
//! raised, never lowered, or an old build would erase the evidence.
//!
//! Upgrades are a non-event: a profile stamped with an older version (or with no
//! stamp at all, i.e. written before this existed) loads without comment.

/// Version of this build, from Cargo.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Split a semver-ish string into comparable numbers. Anything after `-` or `+`
/// (pre-release, build metadata) is ignored, and missing parts count as 0, so
/// "0.4" parses as 0.4.0. Returns `None` if there is no leading number at all.
fn parts(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim();
    let core = core.split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.');
    let major: u64 = it.next()?.trim().parse().ok()?;
    let minor: u64 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let patch: u64 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Is `stored` a strictly newer version than this build?
///
/// An empty or unparseable stamp is treated as "not newer": profiles written
/// before stamping existed must keep loading, and a corrupt stamp shouldn't
/// lock someone out of their own configuration.
pub fn is_newer_than_current(stored: &str) -> bool {
    match (parts(stored), parts(CURRENT)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The higher of the two versions, as a string. Used when saving, so the stamp
/// tracks the newest build that ever touched the profile — including when an
/// older build saves it after being acknowledged.
pub fn higher(stored: &str, current: &str) -> String {
    match (parts(stored), parts(current)) {
        (Some(a), Some(b)) if a >= b => stored.to_string(),
        (Some(_), None) => stored.to_string(),
        _ => current.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_and_decorated_versions() {
        assert_eq!(parts("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parts("0.4"), Some((0, 4, 0)));
        assert_eq!(parts("2"), Some((2, 0, 0)));
        assert_eq!(parts("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parts("1.2.3+build7"), Some((1, 2, 3)));
        assert_eq!(parts(""), None);
        assert_eq!(parts("nonsense"), None);
    }

    #[test]
    fn only_a_strictly_newer_stamp_counts() {
        // Compared against this build, whatever it happens to be.
        let (ma, mi, pa) = parts(CURRENT).expect("own version parses");
        let newer = format!("{ma}.{mi}.{}", pa + 1);
        let older = format!("{ma}.{}.{pa}", mi.saturating_sub(1));

        assert!(is_newer_than_current(&newer));
        assert!(!is_newer_than_current(CURRENT), "same version is fine");
        if mi > 0 {
            assert!(!is_newer_than_current(&older), "upgrades never warn");
        }
    }

    #[test]
    fn unstamped_and_broken_versions_never_warn() {
        assert!(!is_newer_than_current(""));
        assert!(!is_newer_than_current("   "));
        assert!(!is_newer_than_current("not-a-version"));
    }

    #[test]
    fn major_and_minor_order_before_patch() {
        assert!(is_newer_than_current("99.0.0"));
        assert_eq!(higher("1.2.3", "1.10.0"), "1.10.0");
        assert_eq!(higher("2.0.0", "1.99.99"), "2.0.0");
    }

    #[test]
    fn higher_keeps_the_newer_stamp_and_fills_in_blanks() {
        assert_eq!(higher("", "0.3.0"), "0.3.0");
        assert_eq!(higher("0.9.0", "0.3.0"), "0.9.0", "an old build must not lower the stamp");
        assert_eq!(higher("0.3.0", "0.3.0"), "0.3.0");
        assert_eq!(higher("garbage", "0.3.0"), "0.3.0");
    }
}
