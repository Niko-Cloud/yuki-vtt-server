//! The headless host: `yuki-vtt-server`, for running without the window (on a home server,
//! say). It listens on this computer only; the tunnel is what lets players in.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use yuki_vtt_server::state::{AppState, Config};
use yuki_vtt_server::{app, db, ids, tunnel};

const HELP: &str = "yuki-vtt-server: hosts Yuki Char Maker tables on this computer

Options:
  --data <folder>     where tables and pictures are kept (default: ./data)
  --port <number>     the port on this computer (default: 4610)
  --origin <url>      a website allowed to use this host, like https://yuki.example
                      (can be given more than once; also YUKI_VTT_ORIGINS, comma separated)
  --tunnel            open a Cloudflare quick tunnel so players anywhere can reach
                      this host (needs the cloudflared program)
  --help              this text";

struct Options {
    data: PathBuf,
    port: u16,
    origins: Vec<String>,
    tunnel: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        data: PathBuf::from("data"),
        port: 4610,
        origins: Vec::new(),
        tunnel: false,
    };
    if let Ok(list) = std::env::var("YUKI_VTT_ORIGINS") {
        options.origins.extend(
            list.split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(String::from),
        );
    }
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--data" => options.data = PathBuf::from(value()?),
            "--port" => {
                options.port = value()?
                    .parse()
                    .map_err(|_| "--port takes a number".to_string())?
            }
            "--origin" => options.origins.push(value()?),
            "--tunnel" => options.tunnel = true,
            "--help" | "-h" => return Err(HELP.to_string()),
            other => return Err(format!("Unknown option {other}\n\n{HELP}")),
        }
    }
    Ok(options)
}

// A secret kept in a file in the data folder, readable by this user only.
fn secret_file(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_secret(path: &Path, value: &str) -> std::io::Result<()> {
    std::fs::write(path, value)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(options).await {
        eprintln!("yuki-vtt-server stopped: {error}");
        std::process::exit(1);
    }
}

async fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&options.data)?;
    // The DM's key is made on the first start and kept; the window app shows it for pairing.
    let key_path = options.data.join("owner-key");
    let owner_key = match secret_file(&key_path)? {
        Some(key) => key,
        None => {
            let key = ids::secret();
            write_secret(&key_path, &key)?;
            println!("A new DM key was made and saved in {}", key_path.display());
            key
        }
    };
    let website_secret = secret_file(&options.data.join("website-secret"))?.map(String::into_bytes);
    if website_secret.is_none() {
        tracing::warn!("not paired with a website yet: character snapshots are taken unsigned");
    }
    if options.origins.is_empty() {
        tracing::warn!("no --origin given: no website's pages can use this host");
    }
    let connection = db::open(&options.data.join("tables.db"))?;
    let state = AppState::new(
        connection,
        Config {
            owner_key_hash: ids::hash(&owner_key),
            website_secret,
            allowed_origins: options.origins,
            data_dir: options.data.clone(),
        },
    );
    // This computer only: players come in through the tunnel, never straight to the port.
    let address = SocketAddr::from(([127, 0, 0, 1], options.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!("hosting on http://{address}");
    // Kept alive for as long as the host runs; the tunnel stops with it.
    let _tunnel = if options.tunnel {
        let program = tunnel::find(&options.data)
            .ok_or("--tunnel needs cloudflared: install it, or put it in the data folder's bin")?;
        Some(tunnel::open(program, options.port))
    } else {
        None
    };
    axum::serve(listener, app(state))
        .with_graceful_shutdown(stopped())
        .await?;
    tracing::info!("stopped");
    Ok(())
}

// Ctrl+C, or the system asking it to stop (a plain `kill`, a service manager, logging out).
// Either way the host stops cleanly and takes the tunnel with it.
async fn stopped() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("a stop signal handler");
        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = interrupt.await;
}
