//! Getting the `cloudflared` program for the tunnel: the official build from Cloudflare's
//! releases on GitHub, kept in the data folder's `bin`. The download is checked against the
//! SHA-256 GitHub lists for that file before it is ever run.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const RELEASE: &str = "https://api.github.com/repos/cloudflare/cloudflared/releases/latest";

/// The file Cloudflare publishes for this computer, or none for one they don't build for.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("cloudflared-linux-amd64"),
        ("linux", "aarch64") => Some("cloudflared-linux-arm64"),
        ("windows", "x86_64") => Some("cloudflared-windows-amd64.exe"),
        ("macos", "x86_64") => Some("cloudflared-darwin-amd64.tgz"),
        ("macos", "aarch64") => Some("cloudflared-darwin-arm64.tgz"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct Release {
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    /// "sha256:..." as GitHub worked it out when the file was published.
    digest: Option<String>,
}

fn file_name() -> &'static str {
    if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    }
}

/// Downloads cloudflared into `<data>/bin` and returns where it is.
pub async fn download(data_dir: &Path) -> Result<PathBuf, String> {
    let wanted = asset_name()
        .ok_or("Cloudflare has no cloudflared for this kind of computer: install it yourself")?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("yuki-vtt-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let unreachable =
        |error: reqwest::Error| format!("Couldn't reach GitHub for cloudflared: {error}");
    let release: Release = http
        .get(RELEASE)
        .send()
        .await
        .map_err(unreachable)?
        .error_for_status()
        .map_err(unreachable)?
        .json()
        .await
        .map_err(unreachable)?;
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == wanted)
        .ok_or("The newest cloudflared release has no file for this computer")?;
    // Only Cloudflare's own release files, whatever the list says.
    if !asset
        .browser_download_url
        .starts_with("https://github.com/cloudflare/cloudflared/releases/download/")
    {
        return Err("The cloudflared download came from an unexpected place".into());
    }
    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or(
            "GitHub lists no checksum for cloudflared, so it can't be checked: install it yourself",
        )?;
    let bytes = http
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(unreachable)?
        .error_for_status()
        .map_err(unreachable)?
        .bytes()
        .await
        .map_err(unreachable)?;
    if hex::encode(Sha256::digest(&bytes)) != expected.to_lowercase() {
        return Err(
            "The cloudflared download doesn't match its checksum: nothing was installed".into(),
        );
    }
    let program = if wanted.ends_with(".tgz") {
        unpack(&bytes)?
    } else {
        bytes.to_vec()
    };
    let failed = |error: std::io::Error| format!("Couldn't save cloudflared: {error}");
    let bin = data_dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(failed)?;
    let path = bin.join(file_name());
    let part = path.with_extension("part");
    std::fs::write(&part, program).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).map_err(failed)?;
    }
    std::fs::rename(&part, &path).map_err(failed)?;
    Ok(path)
}

// The macOS download is a .tgz with the one program in it.
fn unpack(archive: &[u8]) -> Result<Vec<u8>, String> {
    let broken =
        |error: std::io::Error| format!("The cloudflared download couldn't be unpacked: {error}");
    let mut files = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in files.entries().map_err(broken)? {
        let mut entry = entry.map_err(broken)?;
        if entry
            .path()
            .map_err(broken)?
            .file_name()
            .is_some_and(|name| name == "cloudflared")
        {
            let mut program = Vec::new();
            entry.read_to_end(&mut program).map_err(broken)?;
            return Ok(program);
        }
    }
    Err("The cloudflared download had no program in it".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_file_for_this_computer() {
        // Every system the app is built for has one.
        assert!(asset_name().is_some());
    }

    // Needs the network: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn downloads_a_cloudflared_that_runs() {
        let data = tempfile::tempdir().unwrap();
        let path = download(data.path()).await.unwrap();
        let version = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&version.stdout).contains("cloudflared version"));
    }
}
