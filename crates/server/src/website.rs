//! The host's side of the Yuki Char Maker website: pairing with the DM's account once, then
//! telling the website where players reach the host each time its tunnel gets an address.

use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;

// The website shows a host as off after a few quiet minutes; this is well inside that.
const CHECK_IN: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(3);

/// What pairing leaves in the data folder.
pub struct Paired {
    pub website: String,
    pub host_key: String,
    pub secret: String,
}

/// The website as the host keeps it: "https://yuki.example", no path, no slash at the end.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("yuki-vtt-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("an HTTP client")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Started {
    code: String,
    poll_token: String,
    expires_in_seconds: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Handoff {
    host_key: String,
    secret: String,
}

#[derive(Deserialize)]
struct Refusal {
    message: Option<String>,
}

async fn refusal(response: reqwest::Response) -> String {
    let status = response.status();
    let message = response
        .json::<Refusal>()
        .await
        .ok()
        .and_then(|r| r.message);
    message.unwrap_or_else(|| format!("the website answered {status}"))
}

/// Pairs with the DM's account: asks the website for a code, shows it, and waits for the DM
/// to type it on the website. `show` is told the code once there is one.
pub async fn pair(
    website: &str,
    name: &str,
    owner_key: &str,
    show: impl FnOnce(&str),
) -> Result<Paired, String> {
    let website =
        origin_of(website).ok_or("The website is an address like https://yuki.example")?;
    let http = client();
    let response = http
        .post(format!("{website}/api/hosts/pair"))
        .json(&json!({ "name": name, "ownerKey": owner_key }))
        .send()
        .await
        .map_err(|error| format!("Couldn't reach {website}: {error}"))?;
    if !response.status().is_success() {
        return Err(refusal(response).await);
    }
    let started: Started = response
        .json()
        .await
        .map_err(|_| "The website's answer made no sense")?;
    show(&started.code);
    let give_up = tokio::time::Instant::now() + Duration::from_secs(started.expires_in_seconds);
    while tokio::time::Instant::now() < give_up {
        tokio::time::sleep(POLL).await;
        let Ok(response) = http
            .get(format!("{website}/api/hosts/pair/{}", started.poll_token))
            .send()
            .await
        else {
            continue;
        };
        match response.status().as_u16() {
            202 => continue,
            200 => {
                let handoff: Handoff = response
                    .json()
                    .await
                    .map_err(|_| "The website's answer made no sense")?;
                return Ok(Paired {
                    website,
                    host_key: handoff.host_key,
                    secret: handoff.secret,
                });
            }
            _ => return Err(refusal(response).await),
        }
    }
    Err("Nobody typed the code in time: pair again".into())
}

/// Keeps the pairing in the data folder: the website, the host's key and the shared secret.
pub fn save(data_dir: &Path, paired: &Paired) -> std::io::Result<()> {
    crate::secrets::write(&data_dir.join("website"), &paired.website)?;
    crate::secrets::write(&data_dir.join("host-key"), &paired.host_key)?;
    crate::secrets::write(&data_dir.join("website-secret"), &paired.secret)
}

/// The pairing kept in the data folder, if the host was paired.
pub fn load(data_dir: &Path) -> std::io::Result<Option<(String, String)>> {
    let website = crate::secrets::read(&data_dir.join("website"))?;
    let key = crate::secrets::read(&data_dir.join("host-key"))?;
    Ok(website.zip(key))
}

/// Tells the website where players reach the host: as soon as the tunnel has an address, and
/// every minute after, so the website shows the host as on.
pub fn check_in(
    website: String,
    host_key: String,
    mut address: watch::Receiver<Option<String>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = client();
        loop {
            let now = address.borrow_and_update().clone();
            if let Some(url) = now {
                let sent = http
                    .put(format!("{website}/api/hosts/me"))
                    .bearer_auth(&host_key)
                    .json(&json!({ "url": url }))
                    .send()
                    .await;
                match sent {
                    Ok(response) if response.status().is_success() => {}
                    Ok(response) => tracing::warn!(
                        "the website didn't take the address: {}",
                        refusal(response).await
                    ),
                    Err(error) => tracing::warn!("couldn't reach the website: {error}"),
                }
            }
            // A new address goes at once; otherwise the next check-in.
            let _ = tokio::time::timeout(CHECK_IN, address.changed()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_websites_origin() {
        assert_eq!(
            origin_of("https://yuki.example/settings").as_deref(),
            Some("https://yuki.example")
        );
        assert_eq!(
            origin_of(" http://192.168.1.150:5173 ").as_deref(),
            Some("http://192.168.1.150:5173")
        );
        assert_eq!(origin_of("yuki.example"), None);
        assert_eq!(origin_of("ftp://yuki.example"), None);
    }
}
