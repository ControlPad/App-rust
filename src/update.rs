//! Update check against the GitHub releases API.
//!
//! Deliberately minimal: Slidr never downloads or installs anything by itself.
//! It asks GitHub for the tag of the newest release, compares it with this
//! build, and — if there is something newer — offers a link that opens
//! `releases/latest` in the browser. That keeps the whole feature to one
//! outbound GET, with no updater process, no elevation and nothing to undo if
//! it goes wrong.
//!
//! The check is opt-out (`Settings::update_check`), runs on a detached thread so
//! a hanging network never delays the window, and treats every failure as
//! "no news": an offline machine or a rate-limited API must not produce an
//! error popup on startup.

use std::time::Duration;

/// Page opened when the user acts on an available update.
pub const RELEASES_URL: &str = "https://github.com/ControlPad/App-rust/releases/latest";

/// API endpoint backing the check. Returns the newest *non-draft, non-release-
/// candidate* release, which is exactly what `releases/latest` shows.
const API_URL: &str = "https://api.github.com/repos/ControlPad/App-rust/releases/latest";

/// GitHub rejects requests without a User-Agent.
const USER_AGENT: &str = concat!("Slidr/", env!("CARGO_PKG_VERSION"), " (+", "https://github.com/ControlPad/App-rust", ")");

/// Read `tag_name` out of a GitHub release payload and normalise it: tags are
/// published as `v0.3.0`, versions compared as `0.3.0`.
fn tag_from_payload(body: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow::anyhow!("release payload has no tag_name"))?;
    Ok(tag.trim().trim_start_matches(['v', 'V']).to_string())
}

/// Ask GitHub for the newest published version, as a bare version string.
fn latest_version() -> anyhow::Result<String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let body = agent
        .get(API_URL)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()?
        .into_string()?;
    tag_from_payload(&body)
}

/// Outcome of one check.
pub enum Outcome {
    /// A newer release exists; carries its version (no leading `v`).
    Available(String),
    /// This build is the newest published one.
    UpToDate,
    /// The check itself failed — offline, rate-limited, unparseable answer.
    Failed(String),
}

/// Run one check. Never panics and never blocks longer than the HTTP timeout.
pub fn check() -> Outcome {
    match latest_version() {
        Ok(latest) if crate::version::is_newer_than_current(&latest) => Outcome::Available(latest),
        Ok(_) => Outcome::UpToDate,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Open the releases page in the user's browser.
pub fn open_releases_page() {
    if let Err(e) = open::that(RELEASES_URL) {
        log::warn!("could not open {RELEASES_URL}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_strips_the_tag() {
        let body = r#"{"tag_name":"v0.4.0","name":"Slidr v0.4.0"}"#;
        assert_eq!(tag_from_payload(body).unwrap(), "0.4.0");
    }

    #[test]
    fn accepts_a_tag_without_the_v_prefix() {
        assert_eq!(tag_from_payload(r#"{"tag_name":"1.2.3"}"#).unwrap(), "1.2.3");
    }

    #[test]
    fn rejects_payloads_without_a_tag() {
        // GitHub answers 404 with a JSON body when a repo has no release yet;
        // that must read as an error, not as "version ''".
        assert!(tag_from_payload(r#"{"message":"Not Found"}"#).is_err());
        assert!(tag_from_payload("not json at all").is_err());
    }

    #[test]
    fn only_a_strictly_newer_tag_is_an_update() {
        // Reuses the profile-stamp comparison, so pre-release suffixes and
        // partial versions behave the same in both places.
        assert!(crate::version::is_newer_than_current("99.0.0"));
        assert!(!crate::version::is_newer_than_current(crate::version::CURRENT));
    }
}
