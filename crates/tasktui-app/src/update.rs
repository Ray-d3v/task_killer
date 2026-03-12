use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::os::windows::process::CommandExt;

pub const GITHUB_REPO: &str = "Ray-d3v/task_killer";
pub const DEFAULT_SERVICE_NAME: &str = "tasktui-service";
const CREATE_NEW_CONSOLE: u32 = 0x00000010;
const GITHUB_API_VERSION: &str = "2022-11-28";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub msi_asset_name: String,
    pub msi_download_url: String,
    pub checksum_download_url: String,
    pub msi_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheck {
    UpToDate {
        current: Version,
    },
    UpdateAvailable {
        current: Version,
        release: ReleaseInfo,
    },
}

#[derive(Debug, Clone)]
pub struct DownloadedRelease {
    pub release: ReleaseInfo,
    pub msi_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version must be valid semver")
}

pub fn check_for_updates() -> Result<UpdateCheck> {
    check_for_updates_for_repo(GITHUB_REPO, &current_version())
}

pub fn check_for_updates_for_repo(repo: &str, current: &Version) -> Result<UpdateCheck> {
    let release = fetch_latest_release(repo)?;
    if release.version > *current {
        Ok(UpdateCheck::UpdateAvailable {
            current: current.clone(),
            release,
        })
    } else {
        Ok(UpdateCheck::UpToDate {
            current: current.clone(),
        })
    }
}

pub fn fetch_latest_release(repo: &str) -> Result<ReleaseInfo> {
    let client = github_client()?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let release = client
        .get(url)
        .with_github_headers()
        .send()
        .context("request latest GitHub release");

    match release.and_then(|response| match response.status() {
        reqwest::StatusCode::NOT_FOUND => {
            Err(anyhow!("no published GitHub release was found for {repo}"))
        }
        status if status.is_success() => response
            .json()
            .context("deserialize latest GitHub release"),
        status => Err(anyhow!("latest GitHub release returned HTTP {status}")),
    }) {
        Ok(release) => release_info_from_response(release),
        Err(error) => latest_release_via_redirect(repo)
            .with_context(|| format!("GitHub API lookup failed: {error:#}")),
    }
}

pub fn download_release_artifacts(release: &ReleaseInfo) -> Result<DownloadedRelease> {
    let client = github_client()?;
    let download_dir = std::env::temp_dir().join(format!("task_killer-update-{}", release.version));
    if download_dir.exists() {
        fs::remove_dir_all(&download_dir)
            .with_context(|| format!("remove temp update dir {}", download_dir.display()))?;
    }
    fs::create_dir_all(&download_dir)
        .with_context(|| format!("create temp update dir {}", download_dir.display()))?;

    let expected_hash = if let Some(hash) = &release.msi_sha256 {
        hash.clone()
    } else {
        let checksum_text = client
            .get(&release.checksum_download_url)
            .with_github_headers()
            .send()
            .context("download SHA256SUMS.txt")?
            .error_for_status()
            .context("SHA256SUMS.txt returned an error status")?
            .text()
            .context("read SHA256SUMS.txt")?;
        parse_sha256sums(&checksum_text, &release.msi_asset_name)?
    };

    let msi_path = download_dir.join(&release.msi_asset_name);
    let mut response = client
        .get(&release.msi_download_url)
        .with_github_headers()
        .send()
        .context("download MSI")?
        .error_for_status()
        .context("MSI download returned an error status")?;
    let mut file =
        File::create(&msi_path).with_context(|| format!("create {}", msi_path.display()))?;
    std::io::copy(&mut response, &mut file).context("write downloaded MSI")?;
    file.flush().context("flush downloaded MSI")?;

    let actual_hash = sha256_file(&msi_path)?;
    if actual_hash != expected_hash {
        bail!(
            "checksum mismatch for {} (expected {}, got {})",
            release.msi_asset_name,
            expected_hash,
            actual_hash
        );
    }

    Ok(DownloadedRelease {
        release: release.clone(),
        msi_path,
    })
}

pub fn launch_updater_for_install(current_exe: &Path) -> Result<()> {
    let updater_path = current_exe.with_file_name("updater.exe");
    if !updater_path.exists() {
        bail!("updater.exe was not found next to {}", current_exe.display());
    }

    let current_pid = std::process::id().to_string();
    let current_version = current_version().to_string();
    Command::new(&updater_path)
        .arg("install")
        .arg("--current-version")
        .arg(current_version)
        .arg("--wait-pid")
        .arg(current_pid)
        .arg("--restart-app-path")
        .arg(current_exe)
        .arg("--service-name")
        .arg(DEFAULT_SERVICE_NAME)
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("launch updater {}", updater_path.display()))?;
    Ok(())
}

fn github_client() -> Result<Client> {
    Client::builder()
        .user_agent(format!("task_killer/{}", current_version()))
        .build()
        .context("build GitHub HTTP client")
}

fn github_redirect_client() -> Result<Client> {
    Client::builder()
        .user_agent(format!("task_killer/{}", current_version()))
        .redirect(Policy::none())
        .build()
        .context("build GitHub redirect HTTP client")
}

fn release_info_from_response(release: GitHubRelease) -> Result<ReleaseInfo> {
    let version = normalize_version(&release.tag_name)?;
    let msi_asset_name = format!("task_killer-{version}-x64.msi");
    let msi_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == msi_asset_name)
        .ok_or_else(|| anyhow!("latest release does not contain {msi_asset_name}"))?;
    let checksum_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS.txt")
        .ok_or_else(|| anyhow!("latest release does not contain SHA256SUMS.txt"))?;

    Ok(ReleaseInfo {
        version,
        msi_asset_name,
        msi_download_url: msi_asset.browser_download_url.clone(),
        checksum_download_url: checksum_asset.browser_download_url.clone(),
        msi_sha256: parse_asset_digest(msi_asset.digest.as_deref())?,
    })
}

fn latest_release_via_redirect(repo: &str) -> Result<ReleaseInfo> {
    let client = github_redirect_client()?;
    let url = format!("https://github.com/{repo}/releases/latest");
    let response = client
        .get(url)
        .send()
        .context("request GitHub releases/latest redirect")?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .ok_or_else(|| anyhow!("GitHub releases/latest did not return a redirect"))?
        .to_str()
        .context("decode GitHub release redirect location")?;
    let tag = location
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("could not parse release tag from redirect {location}"))?;
    latest_release_from_tag(repo, tag)
}

fn latest_release_from_tag(repo: &str, tag: &str) -> Result<ReleaseInfo> {
    let version = normalize_version(tag)?;
    let tag_name = if tag.starts_with('v') {
        tag.to_string()
    } else {
        format!("v{tag}")
    };
    let msi_asset_name = format!("task_killer-{version}-x64.msi");
    let checksum_asset_name = "SHA256SUMS.txt".to_string();
    Ok(ReleaseInfo {
        version,
        msi_asset_name: msi_asset_name.clone(),
        msi_download_url: format!(
            "https://github.com/{repo}/releases/download/{tag_name}/{msi_asset_name}"
        ),
        checksum_download_url: format!(
            "https://github.com/{repo}/releases/download/{tag_name}/{checksum_asset_name}"
        ),
        msi_sha256: None,
    })
}

fn normalize_version(tag_name: &str) -> Result<Version> {
    let trimmed = tag_name.trim();
    let normalized = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Version::parse(normalized).with_context(|| format!("parse release version from tag {tag_name}"))
}

fn parse_asset_digest(digest: Option<&str>) -> Result<Option<String>> {
    let Some(digest) = digest else {
        return Ok(None);
    };
    let trimmed = digest.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value = trimmed
        .strip_prefix("sha256:")
        .ok_or_else(|| anyhow!("unsupported release asset digest format: {trimmed}"))?;
    Ok(Some(value.to_ascii_lowercase()))
}

fn parse_sha256sums(text: &str, file_name: &str) -> Result<String> {
    for line in BufReader::new(text.as_bytes()).lines() {
        let line = line.context("read SHA256SUMS.txt line")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let hash = parts.next().unwrap_or_default();
        let file = parts.next().unwrap_or_default().trim_start_matches('*');
        if file == file_name {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(anyhow!("SHA256SUMS.txt does not contain {file_name}"))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn quote_windows_arg(value: &OsString) -> String {
    let text = value.to_string_lossy();
    if text.is_empty() {
        return "\"\"".into();
    }
    if !text.contains([' ', '\t', '"']) {
        return text.into_owned();
    }
    let escaped = text.replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub fn github_api_headers() -> [(&'static str, &'static str); 2] {
    [
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", GITHUB_API_VERSION),
    ]
}

trait RequestBuilderExt {
    fn with_github_headers(self) -> Self;
}

impl RequestBuilderExt for reqwest::blocking::RequestBuilder {
    fn with_github_headers(self) -> Self {
        let mut request = self;
        for (name, value) in github_api_headers() {
            request = request.header(name, value);
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_v_prefix_from_tags() {
        assert_eq!(
            normalize_version("v0.1.1").expect("version"),
            Version::parse("0.1.1").expect("semver")
        );
    }

    #[test]
    fn parses_sha256sums_entries() {
        let hash = parse_sha256sums(
            "abcd1234 *task_killer-0.1.1-x64.msi\nffff0000 *other.zip\n",
            "task_killer-0.1.1-x64.msi",
        )
        .expect("hash");
        assert_eq!(hash, "abcd1234");
    }

    #[test]
    fn release_info_selects_expected_assets() {
        let info = release_info_from_response(GitHubRelease {
            tag_name: "v0.1.1".into(),
            assets: vec![
                GitHubAsset {
                    name: "task_killer-0.1.1-x64.msi".into(),
                    browser_download_url: "https://example.com/a.msi".into(),
                    digest: Some("sha256:1234abcd".into()),
                },
                GitHubAsset {
                    name: "SHA256SUMS.txt".into(),
                    browser_download_url: "https://example.com/SHA256SUMS.txt".into(),
                    digest: None,
                },
            ],
        })
        .expect("release");
        assert_eq!(info.version, Version::parse("0.1.1").expect("semver"));
        assert_eq!(info.msi_asset_name, "task_killer-0.1.1-x64.msi");
        assert_eq!(info.msi_sha256, Some("1234abcd".into()));
    }

    #[test]
    fn redirect_fallback_builds_expected_asset_urls() {
        let repo = "Ray-d3v/task_killer";
        let release = latest_release_from_tag(repo, "v0.1.2").expect("release");
        assert_eq!(release.version, Version::parse("0.1.2").expect("semver"));
        assert_eq!(
            release.msi_download_url,
            "https://github.com/Ray-d3v/task_killer/releases/download/v0.1.2/task_killer-0.1.2-x64.msi"
        );
        assert_eq!(release.msi_sha256, None);
    }

    #[test]
    fn compares_versions_for_updates() {
        let current = Version::parse("0.1.0").expect("semver");
        let release = ReleaseInfo {
            version: Version::parse("0.1.1").expect("semver"),
            msi_asset_name: "task_killer-0.1.1-x64.msi".into(),
            msi_download_url: "https://example.com/a.msi".into(),
            checksum_download_url: "https://example.com/SHA256SUMS.txt".into(),
            msi_sha256: Some("1234abcd".into()),
        };
        let check = if release.version > current {
            UpdateCheck::UpdateAvailable {
                current,
                release: release.clone(),
            }
        } else {
            UpdateCheck::UpToDate {
                current: Version::parse("0.1.0").expect("semver"),
            }
        };
        assert!(matches!(check, UpdateCheck::UpdateAvailable { .. }));
    }
}
