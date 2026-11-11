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

/// Pairs in one step, from a link the website gave the DM while signed in: the link's token
/// says which account, so there is no code to type.
pub async fn pair_with_link(
    website: &str,
    token: &str,
    name: &str,
    owner_key: &str,
) -> Result<Paired, String> {
    let website = origin_of(website).ok_or("The link's website is not an address")?;
    let response = client()
        .post(format!("{website}/api/hosts/pair/link"))
        .json(&json!({ "token": token, "name": name, "ownerKey": owner_key }))
        .send()
        .await
        .map_err(|error| format!("Couldn't reach {website}: {error}"))?;
    if !response.status().is_success() {
        return Err(refusal(response).await);
    }
    let handoff: Handoff = response
        .json()
        .await
        .map_err(|_| "The website's answer made no sense")?;
    Ok(Paired {
        website,
        host_key: handoff.host_key,
        secret: handoff.secret,
    })
}

/// What a `yukivtt://` link asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Link {
    /// Start hosting.
    Start,
    /// Pair with this website, for the account the token belongs to.
    Pair { website: String, token: String },
}

/// Reads a `yukivtt://...` link; anything else is ignored.
pub fn read_link(url: &str) -> Option<Link> {
    let rest = url.strip_prefix("yukivtt://")?;
    let (what, query) = rest.split_once('?').unwrap_or((rest, ""));
    let what = what.trim_end_matches('/');
    let param = |name: &str| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| decode(value))
        })
    };
    match what {
        "start" => Some(Link::Start),
        "pair" => {
            let website = origin_of(&param("website")?)?;
            let token = param("token").filter(|t| !t.is_empty() && t.len() <= 200)?;
            Some(Link::Pair { website, token })
        }
        _ => None,
    }
}

// Undoes the %xx escapes a link's values come with.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
    fn reads_the_links_the_website_opens() {
        assert_eq!(read_link("yukivtt://start"), Some(Link::Start));
        assert_eq!(read_link("yukivtt://start/"), Some(Link::Start));
        assert_eq!(
            read_link("yukivtt://pair?website=https%3A%2F%2Fyuki.example&token=abc123"),
            Some(Link::Pair {
                website: "https://yuki.example".into(),
                token: "abc123".into()
            })
        );
        assert_eq!(
            read_link("yukivtt://pair?website=https%3A%2F%2Fyuki.example"),
            None
        );
        assert_eq!(
            read_link("yukivtt://pair?website=javascript%3Aalert(1)&token=x"),
            None
        );
        assert_eq!(read_link("https://yuki.example/start"), None);
        assert_eq!(read_link("yukivtt://delete-everything"), None);
    }

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
