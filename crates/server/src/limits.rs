//! How often one visitor may ask the host for things. The host is on the internet through its
//! tunnel, so a stranger hammering it shouldn't be able to slow down the game or fill the disk.
//! Each visitor gets a count per minute for each kind of request; a game night stays far below.

use crate::error::AppError;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
// Forget visitors after this many, oldest windows first, so the list can't grow without end.
const MOST_VISITORS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Pictures: the biggest requests.
    Upload,
    /// Joining a table, making one: what a stranger would try.
    Join,
    /// Moves, rolls, chat.
    Write,
    /// Rulers and pointer trails, many a second while someone measures or points.
    Mark,
    /// Looking: the table, pictures, the live channel.
    Read,
}

impl Kind {
    /// Per visitor, per minute.
    pub fn limit(self) -> u32 {
        match self {
            Kind::Upload => 30,
            Kind::Join => 20,
            Kind::Write => 300,
            Kind::Mark => 1200,
            Kind::Read => 900,
        }
    }

    pub fn of(method: &Method, path: &str) -> Kind {
        let upload = (method == Method::PUT && path.ends_with("/image"))
            || (method == Method::POST && path.ends_with("/assets"));
        if upload {
            Kind::Upload
        } else if method == Method::POST
            && (path.starts_with("/api/join/") || path == "/api/tables")
        {
            Kind::Join
        } else if method == Method::POST && path.ends_with("/mark") {
            Kind::Mark
        } else if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
            Kind::Read
        } else {
            Kind::Write
        }
    }
}

// Per visitor and kind: when their minute started, and how many so far.
type Counts = HashMap<(String, Kind), (Instant, u32)>;

#[derive(Clone, Default)]
pub struct Limits {
    seen: Arc<Mutex<Counts>>,
}

impl Limits {
    /// Counts one request; false when the visitor is over the limit for this minute.
    pub fn allow(&self, visitor: &str, kind: Kind) -> bool {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.len() >= MOST_VISITORS {
            seen.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        }
        let entry = seen.entry((visitor.to_string(), kind)).or_insert((now, 0));
        if now.duration_since(entry.0) >= WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= kind.limit()
    }
}

/// Who is asking. Through the tunnel every request comes from this computer, and Cloudflare
/// says who sent it; anything else is taken by its own address. The header is only believed
/// from this computer, so nobody else can pretend to be someone new each time.
pub fn visitor(request: &Request) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let from_tunnel = peer.is_none_or(|ip| ip.is_loopback());
    let forwarded = request
        .headers()
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .filter(|_| from_tunnel);
    match (forwarded, peer) {
        (Some(ip), _) => ip.to_string(),
        (None, Some(ip)) => ip.to_string(),
        (None, None) => "local".into(),
    }
}

pub async fn check(State(limits): State<Limits>, request: Request, next: Next) -> Response {
    let kind = Kind::of(request.method(), request.uri().path());
    if limits.allow(&visitor(&request), kind) {
        return next.run(request).await;
    }
    let mut refused = AppError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many requests: wait a minute and try again",
    )
    .into_response();
    refused
        .headers_mut()
        .insert("retry-after", "60".parse().expect("a header value"));
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_requests_by_what_they_cost() {
        assert_eq!(
            Kind::of(&Method::PUT, "/api/tables/t/scenes/s/image"),
            Kind::Upload
        );
        assert_eq!(
            Kind::of(&Method::POST, "/api/tables/t/assets"),
            Kind::Upload
        );
        assert_eq!(Kind::of(&Method::POST, "/api/join/abc"), Kind::Join);
        assert_eq!(Kind::of(&Method::GET, "/api/join/abc"), Kind::Read);
        assert_eq!(Kind::of(&Method::POST, "/api/tables/t/roll"), Kind::Write);
    }

    #[test]
    fn counts_each_visitor_on_their_own() {
        let limits = Limits::default();
        for _ in 0..Kind::Join.limit() {
            assert!(limits.allow("1.2.3.4", Kind::Join));
        }
        assert!(!limits.allow("1.2.3.4", Kind::Join));
        assert!(limits.allow("5.6.7.8", Kind::Join));
        assert!(limits.allow("1.2.3.4", Kind::Read));
    }
}
