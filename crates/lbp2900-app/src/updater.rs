//! Self-update from the GitHub releases of the driver repository.

use std::io::{self, Read, Write};
use std::path::Path;

use serde::Deserialize;

/// `owner/name` of the GitHub repository; CI sets `LBP2900_REPO` from `GITHUB_REPOSITORY`.
pub const REPO: &str = match option_env!("LBP2900_REPO") {
    Some(r) => r,
    None => "mk-manishkandel/Canon-LBP-2900-Driver-Mac-MSeries",
};
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub pkg_url: String,
    pub page_url: String,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    assets: Vec<GhAsset>,
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// Whether `candidate` (e.g. `v0.2.0`) is newer than `current` (e.g. `0.1.0`).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(&format!("lbp2900-app/{CURRENT}"))
        .build()
}

/// Latest published release that carries a `.pkg` asset.
pub fn latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let rel: GhRelease = agent()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("cannot reach GitHub: {e}"))?
        .into_json()
        .map_err(|e| format!("unexpected GitHub reply: {e}"))?;
    if rel.draft {
        return Err("latest release is a draft".into());
    }
    let pkg = rel
        .assets
        .iter()
        .find(|a| a.name.ends_with(".pkg"))
        .ok_or("latest release has no .pkg installer")?;
    Ok(Release { version: rel.tag_name, pkg_url: pkg.browser_download_url.clone(), page_url: rel.html_url })
}

pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    let resp = agent().get(url).call().map_err(|e| format!("download failed: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e: io::Error| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }
}
