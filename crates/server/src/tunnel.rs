//! The tunnel that lets players in: Cloudflare's free "quick tunnel", run with the official
//! `cloudflared` program. It needs no account and no router setup; it hands out a new
//! https://....trycloudflare.com address each time it starts.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

/// Where `cloudflared` is: in the data folder's `bin` (where the window app puts it), or on
/// the system's PATH.
pub fn find(data_dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };
    let local = data_dir.join("bin").join(name);
    if local.is_file() {
        return Some(local);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    })
}

/// The address in a line of cloudflared's output, if the line has it.
pub fn address_in(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|')
        .unwrap_or(rest.len());
    let address = &rest[..end];
    address
        .ends_with(".trycloudflare.com")
        .then(|| address.to_string())
}

/// Keeps a tunnel open to `port` for as long as the host runs, starting it again if it stops.
/// The receiver always holds the address that works now, or `None` while there is none.
///
/// Stopping the returned task closes the tunnel: cloudflared goes with it.
pub fn open(
    program: PathBuf,
    port: u16,
) -> (watch::Receiver<Option<String>>, tokio::task::JoinHandle<()>) {
    let (address, receiver) = watch::channel(None);
    let task = tokio::spawn(async move {
        let mut wait = Duration::from_secs(2);
        loop {
            match run(&program, port, &address).await {
                Ok(()) => tracing::warn!("the tunnel stopped; starting it again"),
                Err(error) => tracing::error!("the tunnel couldn't start: {error}"),
            }
            address.send_replace(None);
            tokio::time::sleep(wait).await;
            // Waits longer each time it fails, up to a minute, so a missing network isn't hammered.
            wait = (wait * 2).min(Duration::from_secs(60));
        }
    });
    (receiver, task)
}

async fn run(
    program: &Path,
    port: u16,
    address: &watch::Sender<Option<String>>,
) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // The tunnel goes when the host does.
        .kill_on_drop(true)
        .spawn()?;
    // cloudflared writes its news, the address among it, to stderr.
    let mut lines = BufReader::new(child.stderr.take().expect("stderr was piped")).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(found) = address_in(&line) {
            tracing::info!("players can reach this host at {found}");
            address.send_replace(Some(found));
        }
    }
    child.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_address_in_cloudflareds_box() {
        let line = "2026-11-10T18:04:22Z INF |  https://robust-welding-newsletter-displays.trycloudflare.com                    |";
        assert_eq!(
            address_in(line).as_deref(),
            Some("https://robust-welding-newsletter-displays.trycloudflare.com")
        );
        assert_eq!(
            address_in("INF Requesting new quick Tunnel on trycloudflare.com..."),
            None
        );
        assert_eq!(
            address_in("see https://developers.cloudflare.com/ for more"),
            None
        );
    }
}
