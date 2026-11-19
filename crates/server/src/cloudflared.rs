//! Getting the `cloudflared` program for the tunnel: one fixed release of Cloudflare's official
//! build from GitHub, kept in the data folder's `bin`. Its SHA-256 is written here, so the
//! download is checked against what this app was built with, not against what a web page says
//! today; a new cloudflared comes with a new version of this app.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The cloudflared release this app uses.
pub const VERSION: &str = "2026.11.1";

// Each file Cloudflare publishes for that release, and its SHA-256.
const FILES: [(&str, &str, &str, &str); 5] = [
    (
        "linux",
        "x86_64",
        "cloudflared-linux-amd64",
        "97bb3b02d23a5980fc9c15a1a270e692da1923f103f920d5b16d4931d5ab55e7",
    ),
    (
        "linux",
        "aarch64",
        "cloudflared-linux-arm64",
        "b155cec3e48e489cfb8e5c602769fb8c771083018d8881d68b4d18a791567c5a",
    ),
    (
        "windows",
        "x86_64",
        "cloudflared-windows-amd64.exe",
        "341deda3db5ec498a0482254d6c38c0b7cf01a30c37bb5dd95d2216d08955e35",
    ),
    (
        "macos",
        "x86_64",
        "cloudflared-darwin-amd64.tgz",
        "5e0403b188dfee71fbd9b995e7dc9910d61b661c52371935ab6475d41185632f",
    ),
    (
        "macos",
        "aarch64",
        "cloudflared-darwin-arm64.tgz",
        "02fe4bcf08f2c09699cff30658228a739a0c47fb75f3653d1b8772a27ade4462",
    ),
];

/// The file Cloudflare publishes for this computer and its SHA-256, or none for a computer
/// they don't build for.
pub fn asset() -> Option<(&'static str, &'static str)> {
    FILES
        .iter()
        .find(|(os, arch, ..)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
        .map(|(_, _, name, sha)| (*name, *sha))
}

/// The file Cloudflare publishes for this computer, or none for one they don't build for.
pub fn asset_name() -> Option<&'static str> {
    asset().map(|(name, _)| name)
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
    let (wanted, expected) = asset()
        .ok_or("Cloudflare has no cloudflared for this kind of computer: install it yourself")?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("yuki-vtt-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let unreachable =
        |error: reqwest::Error| format!("Couldn't reach GitHub for cloudflared: {error}");
    let address =
        format!("https://github.com/cloudflare/cloudflared/releases/download/{VERSION}/{wanted}");
    let bytes = http
        .get(&address)
        .send()
        .await
        .map_err(unreachable)?
        .error_for_status()
        .map_err(unreachable)?
        .bytes()
        .await
        .map_err(unreachable)?;
    if hex::encode(Sha256::digest(&bytes)) != expected {
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
    fn knows_the_file_and_its_checksum_for_this_computer() {
        // Every system the app is built for has one.
        assert!(asset_name().is_some());
        for (_, _, name, sha) in FILES {
            assert!(name.starts_with("cloudflared-"), "{name}");
            assert_eq!(sha.len(), 64, "{name}");
            assert!(
                sha.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
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
