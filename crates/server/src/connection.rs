//! A quick look at the DM's connection once hosting starts: players reach the host through the
//! tunnel, the host reaches the website, and the upload is enough for a table. Everything a
//! table sends goes out over the DM's upload, so a slow one is worth saying out loud.

use serde::Serialize;
use std::time::{Duration, Instant};

// Under this, maps and pictures come slowly to a full table.
const SLOW_MBPS: f64 = 5.0;
// What the upload test sends: enough to measure, small enough not to hurt.
const SAMPLE: usize = 2 * 1024 * 1024;
const SPEED_TEST: &str = "https://speed.cloudflare.com/__up";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    /// Players can reach the host through its tunnel.
    pub tunnel: Option<bool>,
    /// The host can reach the paired website.
    pub website: Option<bool>,
    /// Roughly how fast this computer sends, in megabits a second.
    pub upload_mbps: Option<f64>,
    /// Slow enough to warn the DM.
    pub slow: bool,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("yuki-vtt-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("a client")
}

async fn answers(http: &reqwest::Client, url: &str) -> bool {
    http.get(url)
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Megabits a second, from how long `bytes` took to go.
pub fn mbps(bytes: usize, took: Duration) -> f64 {
    let seconds = took.as_secs_f64().max(0.001);
    (bytes as f64 * 8.0 / 1_000_000.0) / seconds
}

async fn upload_speed(http: &reqwest::Client) -> Option<f64> {
    let started = Instant::now();
    let sent = http
        .post(SPEED_TEST)
        .body(vec![0u8; SAMPLE])
        .send()
        .await
        .ok()?;
    sent.status()
        .is_success()
        .then(|| mbps(SAMPLE, started.elapsed()))
}

/// Checks the tunnel (its address, when there is one), the website (when paired) and the
/// upload. Each part is left unknown when it can't be tried.
pub async fn check(tunnel: Option<&str>, website: Option<&str>) -> Connection {
    let http = client();
    let tunnel = match tunnel {
        Some(url) => Some(answers(&http, &format!("{url}/api/health")).await),
        None => None,
    };
    let website = match website {
        Some(site) => Some(answers(&http, site).await),
        None => None,
    };
    let upload_mbps = upload_speed(&http).await.map(|m| (m * 10.0).round() / 10.0);
    Connection {
        tunnel,
        website,
        upload_mbps,
        slow: upload_mbps.is_some_and(|m| m < SLOW_MBPS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works_out_megabits_a_second() {
        assert_eq!(mbps(2_000_000, Duration::from_secs(2)), 8.0);
        assert!(mbps(SAMPLE, Duration::from_secs(10)) < SLOW_MBPS);
    }

    // Needs the network: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn measures_this_computers_upload() {
        let found = check(None, None).await;
        assert!(found.upload_mbps.is_some_and(|m| m > 0.0), "{found:?}");
    }
}
