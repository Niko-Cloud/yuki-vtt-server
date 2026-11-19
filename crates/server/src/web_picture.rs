//! Pictures fetched from a web address for the library, as on the website. The host sits on
//! the DM's own network, so it must never be the way in to it: only addresses on the open
//! internet are fetched, checked for every name and every redirect.

use crate::access::{self, Credentials};
use crate::error::{AppError, Result};
use crate::pictures::picture_type;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

// Shrunk in the browser before it joins the library, so it may come heavier than an upload.
pub const WEB_PICTURE_LIMIT: usize = 12 * 1024 * 1024;
const REDIRECTS: usize = 3;
const TIMEOUT: Duration = Duration::from_secs(10);

fn v4_is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

// Of IPv6, only global unicast (2000::/3), without the ranges inside it that tunnel to IPv4
// or are kept for documentation.
fn v6_is_public(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    let global = (first & 0xe000) == 0x2000;
    let tunnels = first == 0x2002 || (first == 0x2001 && second < 0x0200);
    let documentation = first == 0x2001 && second == 0x0db8;
    global && !tunnels && !documentation
}

/// Whether an address is on the open internet, not this computer or a private network.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_is_public(ip),
        IpAddr::V6(ip) => v6_is_public(ip),
    }
}

/// What may be fetched: the real host allows public addresses on the usual ports; the tests
/// let in their own site.
#[derive(Clone)]
pub struct Rules {
    pub allowed: fn(IpAddr) -> bool,
    pub ports: Vec<Option<u16>>,
}

impl Default for Rules {
    fn default() -> Self {
        // The usual ports only: anything else is more likely a service than a picture.
        Rules {
            allowed: is_public,
            ports: vec![None, Some(80), Some(443)],
        }
    }
}

// Names resolve here, so the connection goes to the very address that was checked.
struct CheckedNames(fn(IpAddr) -> bool);

impl Resolve for CheckedNames {
    fn resolve(&self, name: Name) -> Resolving {
        let allowed = self.0;
        Box::pin(async move {
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if found.is_empty() || found.iter().any(|a| !allowed(a.ip())) {
                return Err(Box::from("That address isn't on the open internet"));
            }
            let addrs: Addrs = Box::new(found.into_iter());
            Ok(addrs)
        })
    }
}

fn check(url: &reqwest::Url, rules: &Rules) -> std::result::Result<(), String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Use a web address that starts with http or https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("A picture's address has no password in it".into());
    }
    if !rules.ports.contains(&url.port()) {
        return Err("Use a web address on the usual port".into());
    }
    // An address written as numbers never reaches the resolver, so it is checked here.
    let host = url.host_str().ok_or("That isn't a web address")?;
    let literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok();
    if literal.is_some_and(|ip| !(rules.allowed)(ip)) {
        return Err("That address isn't on the open internet".into());
    }
    Ok(())
}

/// Fetches a picture from a web address: its bytes and its type, told by its first bytes.
pub async fn fetch(address: &str, rules: &Rules) -> Result<(Vec<u8>, &'static str)> {
    let url = reqwest::Url::parse(address.trim())
        .map_err(|_| AppError::bad("That isn't a web address"))?;
    check(&url, rules).map_err(AppError::bad)?;
    let redirects = rules.clone();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .dns_resolver(Arc::new(CheckedNames(rules.allowed)))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > REDIRECTS {
                return attempt.error("That address sends you round too many times");
            }
            match check(attempt.url(), &redirects) {
                Ok(()) => attempt.follow(),
                Err(why) => attempt.error(why),
            }
        }))
        .user_agent("Mozilla/5.0 (picture fetch)")
        .build()
        .map_err(|_| AppError::bad("Couldn't reach that address"))?;
    let mut response = client
        .get(url)
        .header(header::ACCEPT, "image/*,video/webm")
        .send()
        .await
        .map_err(|error| AppError::bad(why_not(&error)))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::bad(format!(
            "That address answered {}, not a picture",
            status.as_u16()
        )));
    }
    let too_big = || {
        AppError::bad(format!(
            "The picture is over {} MB",
            WEB_PICTURE_LIMIT / (1024 * 1024)
        ))
    };
    if response
        .content_length()
        .is_some_and(|n| n > WEB_PICTURE_LIMIT as u64)
    {
        return Err(too_big());
    }
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AppError::bad("The picture stopped coming"))?
    {
        data.extend_from_slice(&chunk);
        if data.len() > WEB_PICTURE_LIMIT {
            return Err(too_big());
        }
    }
    let mime = picture_type(&data, WEB_PICTURE_LIMIT, WEB_PICTURE_LIMIT)?;
    Ok((data, mime))
}

// The refusal the resolver or the redirect check gave, or a plain one.
fn why_not(error: &reqwest::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(cause) = source {
        let text = cause.to_string();
        if text.starts_with("That ") || text.starts_with("Use ") || text.starts_with("A picture") {
            return text;
        }
        source = cause.source();
    }
    if error.is_timeout() {
        "That address took too long to answer".into()
    } else {
        "Couldn't reach that address".into()
    }
}

#[derive(Deserialize)]
pub struct WebPicture {
    url: String,
}

/// `POST /api/tables/{id}/web-picture`: a picture fetched from a web address, handed back to
/// the browser to shrink, measure and upload like any other. Whoever may add pictures may
/// fetch one.
pub async fn web_picture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<WebPicture>,
) -> Result<Response> {
    let access = access::at_table(&state, &id, &Credentials::from(&headers, None))?;
    if !access.may("tokens") {
        return Err(AppError::not_found("The DM hasn't let you add pictures"));
    }
    if body.url.len() > 2000 {
        return Err(AppError::bad("That address is too long"));
    }
    let (data, mime) = fetch(&body.url, &Rules::default()).await?;
    Ok((
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, "no-store".into()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        data,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_the_open_internet_from_private_networks() {
        for open in ["93.184.215.14", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(open.parse().unwrap()), "{open}");
        }
        for closed in [
            "127.0.0.1",
            "10.1.2.3",
            "172.20.0.1",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fd00::1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2001:db8::1",
        ] {
            assert!(!is_public(closed.parse().unwrap()), "{closed}");
        }
    }
}
