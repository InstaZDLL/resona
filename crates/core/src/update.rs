//! Is there a newer Spytify? Asks GitHub for the latest release of the
//! repository (replaces the C# version's `EspionSpotify.Updater`, which
//! unzipped the release over the install; here the installer does that).

use std::time::Duration;

use serde::Deserialize;

const LATEST_RELEASE: &str = "https://api.github.com/repos/InstaZDLL/spytify/releases/latest";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `0.2.0`, without the tag's `v`.
    pub version: String,
    /// The release page, to read the notes and download the installer.
    pub url: String,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The latest release if it is newer than `current` (`CARGO_PKG_VERSION`).
/// `Ok(None)` when up to date or when the repository has no release yet.
pub fn newer_release(current: &str) -> Result<Option<Release>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("Spytify/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| e.to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let latest: LatestRelease = response
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    Ok(pick(current, latest))
}

fn pick(current: &str, latest: LatestRelease) -> Option<Release> {
    if latest.draft || latest.prerelease {
        return None;
    }
    let version = latest.tag_name.trim_start_matches('v').to_owned();
    is_newer(&version, current).then_some(Release {
        version,
        url: latest.html_url,
    })
}

/// `a` > `b`, comparing dotted numbers (`0.10.0` > `0.9.3`). A suffix
/// (`1.0.0-rc1`) counts as older than the plain version.
fn is_newer(a: &str, b: &str) -> bool {
    parse(a) > parse(b)
}

fn parse(version: &str) -> (Vec<u64>, bool) {
    let (numbers, stable) = match version.split_once('-') {
        Some((numbers, _)) => (numbers, false),
        None => (version, true),
    };
    let mut parts: Vec<u64> = numbers.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    while parts.len() < 3 {
        parts.push(0);
    }
    (parts, stable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(tag: &str) -> LatestRelease {
        LatestRelease {
            tag_name: tag.into(),
            html_url: format!("https://github.com/InstaZDLL/spytify/releases/tag/{tag}"),
            draft: false,
            prerelease: false,
        }
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
        assert!(is_newer("0.1.0", "0.1.0-rc1"));
    }

    #[test]
    fn only_a_newer_stable_release_is_offered() {
        let release = pick("0.1.0", latest("v0.2.0")).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert!(release.url.ends_with("/v0.2.0"));
        assert_eq!(pick("0.2.0", latest("v0.2.0")), None);
        let mut beta = latest("v0.3.0");
        beta.prerelease = true;
        assert_eq!(pick("0.2.0", beta), None);
    }
}
