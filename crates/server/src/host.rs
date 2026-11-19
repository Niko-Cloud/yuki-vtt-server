//! The host as something to switch on and off: what the `yuki-vtt-server` program runs until
//! it is told to stop, and what the window app starts and stops from its tray.

use crate::state::{AppState, Config};
use crate::{app, db, ids, secrets, tunnel, website};
use serde::Serialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

pub struct Options {
    /// Where tables, pictures and the host's keys are kept.
    pub data: PathBuf,
    /// The port on this computer.
    pub port: u16,
    /// Open a Cloudflare quick tunnel, so players anywhere can reach the host.
    pub tunnel: bool,
    /// Websites allowed to use the host, beside the paired one.
    pub origins: Vec<String>,
}

/// A host that is on. Dropping it without `stop` still closes the tunnel.
pub struct Running {
    pub state: AppState,
    /// The tunnel's address as it is now; none without a tunnel.
    pub address: Option<watch::Receiver<Option<String>>>,
    pub port: u16,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<()>,
    helpers: Vec<JoinHandle<()>>,
}

/// The DM's key, made on the first start and kept; the website gets it when pairing.
pub fn owner_key(data: &Path) -> std::io::Result<String> {
    let path = data.join("owner-key");
    if let Some(key) = secrets::read(&path)? {
        return Ok(key);
    }
    let key = ids::secret();
    secrets::write(&path, &key)?;
    Ok(key)
}

/// Switches the host on. It listens on this computer only; players come in through the
/// tunnel, never straight to the port.
pub async fn start(mut options: Options) -> Result<Running, String> {
    let problem = |what: &str| {
        let what = what.to_string();
        move |error: std::io::Error| format!("{what}: {error}")
    };
    std::fs::create_dir_all(&options.data).map_err(problem("Couldn't make the data folder"))?;
    let key = owner_key(&options.data).map_err(problem("Couldn't keep the DM's key"))?;
    let paired = website::load(&options.data).map_err(problem("Couldn't read the pairing"))?;
    let website_secret = secrets::read(&options.data.join("website-secret"))
        .map_err(problem("Couldn't read the pairing"))?
        .map(String::into_bytes);
    match &paired {
        // The paired website's pages may always use this host.
        Some((site, _)) => options.origins.push(site.clone()),
        None => tracing::warn!("not paired with a website yet"),
    }
    if website_secret.is_none() {
        tracing::warn!("character snapshots are taken unsigned until the host is paired");
    }
    let connection = db::open(&options.data.join("tables.db"))
        .map_err(|error| format!("Couldn't open the tables: {error}"))?;
    crate::pictures::gather_library(&options.data, &connection)
        .map_err(|error| format!("Couldn't gather the picture library: {error}"))?;
    let state = AppState::new(
        connection,
        Config {
            owner_key_hash: ids::hash(&key),
            website_secret,
            allowed_origins: options.origins,
            data_dir: options.data.clone(),
        },
    );
    let address = SocketAddr::from(([127, 0, 0, 1], options.port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| {
            format!(
                "Port {} is taken ({error}): is the host already running?",
                options.port
            )
        })?;
    tracing::info!("hosting on http://{address}");

    // Pictures the DM copied in, renamed or deleted while the host was off count from now on.
    crate::library::rescan(&state);
    let mut helpers = vec![crate::library::watch(state.clone())];
    let tunnel_address = if options.tunnel {
        let program =
            tunnel::find(&options.data).ok_or("The tunnel needs the cloudflared program")?;
        let (address, task) = tunnel::open(program, options.port);
        helpers.push(task);
        // The website learns each new address, so the DM's invite links keep working.
        if let Some((site, key)) = paired {
            helpers.push(website::check_in(site, key, address.clone()));
        }
        Some(address)
    } else {
        None
    };

    let (stop, stopped) = oneshot::channel();
    let router = app(state.clone());
    let server = tokio::spawn(async move {
        // The visitor's address, for the rate limits.
        let served = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await;
        if let Err(error) = served {
            tracing::error!("the host stopped by itself: {error}");
        }
    });
    Ok(Running {
        state,
        address: tunnel_address,
        port: options.port,
        stop: Some(stop),
        server,
        helpers,
    })
}

impl Running {
    /// Where players reach the host right now.
    pub fn public_address(&self) -> Option<String> {
        self.address
            .as_ref()
            .and_then(|address| address.borrow().clone())
    }

    /// Switches the host off: the tunnel closes, open screens are let go, the port is freed.
    pub async fn stop(mut self) {
        for helper in self.helpers.drain(..) {
            helper.abort();
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Screens holding the live channel open would keep the server waiting; a moment is
        // enough for the rest.
        let waited =
            tokio::time::timeout(std::time::Duration::from_secs(3), &mut self.server).await;
        if waited.is_err() {
            self.server.abort();
        }
        tracing::info!("stopped");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        for helper in &self.helpers {
            helper.abort();
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCard {
    pub id: String,
    pub name: String,
    pub seats: i64,
    pub updated_at: String,
}

/// The tables kept in a data folder, newest first, for the window's list. Works whether the
/// host is on or off.
pub fn tables(data: &Path) -> Result<Vec<TableCard>, String> {
    let path = data.join("tables.db");
    if !path.exists() {
        return Ok(vec![]);
    }
    let read = || -> rusqlite::Result<Vec<TableCard>> {
        let db = db::open(&path)?;
        let mut rows = db.prepare(
            "SELECT t.id, t.name, t.updated_at, (SELECT COUNT(*) FROM seats s WHERE s.table_id = t.id)
             FROM tables t ORDER BY t.updated_at DESC",
        )?;
        rows.query_map([], |row| {
            Ok(TableCard {
                id: row.get(0)?,
                name: row.get(1)?,
                updated_at: ids::iso(row.get(2)?),
                seats: row.get(3)?,
            })
        })?
        .collect()
    };
    read().map_err(|error| format!("Couldn't read the tables: {error}"))
}
