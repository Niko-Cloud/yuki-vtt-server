//! The headless host: `yuki-vtt-server`, for running without the window (on a home server,
//! say). It listens on this computer only; the tunnel is what lets players in.

use std::path::PathBuf;
use yuki_vtt_server::{cloudflared, host, tunnel, website};

const HELP: &str = "yuki-vtt-server: hosts Yuki Char Maker tables on this computer

  yuki-vtt-server [options]                      host tables
  yuki-vtt-server pair --website <url> [options] connect this host to your account, once

Options:
  --data <folder>     where tables and pictures are kept (default: ./data)
  --port <number>     the port on this computer (default: 4610)
  --tunnel            open a Cloudflare quick tunnel so players anywhere can reach
                      this host (cloudflared is downloaded on first use)
  --origin <url>      another website allowed to use this host; the paired one always is
                      (can be given more than once; also YUKI_VTT_ORIGINS, comma separated)
  --website <url>     pair: the Yuki Char Maker website, like https://yuki.example
  --name <text>       pair: what the website calls this host (default: this computer's name)
  --help              this text";

struct Options {
    pair: bool,
    data: PathBuf,
    port: u16,
    origins: Vec<String>,
    tunnel: bool,
    website: Option<String>,
    name: Option<String>,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        pair: false,
        data: PathBuf::from("data"),
        port: 4610,
        origins: Vec::new(),
        tunnel: false,
        website: None,
        name: None,
    };
    if let Ok(list) = std::env::var("YUKI_VTT_ORIGINS") {
        options.origins.extend(
            list.split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(String::from),
        );
    }
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("pair") {
        args.next();
        options.pair = true;
    }
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
            "--website" => options.website = Some(value()?),
            "--name" => options.name = Some(value()?),
            "--help" | "-h" => return Err(HELP.to_string()),
            other => return Err(format!("Unknown option {other}\n\n{HELP}")),
        }
    }
    Ok(options)
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
    let done = if options.pair {
        pair(options).await
    } else {
        run(options).await
    };
    if let Err(error) = done {
        eprintln!("yuki-vtt-server stopped: {error}");
        std::process::exit(1);
    }
}

async fn pair(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let site = options
        .website
        .ok_or("pair needs --website, like --website https://yuki.example")?;
    std::fs::create_dir_all(&options.data)?;
    let key = host::owner_key(&options.data)?;
    let name = options
        .name
        .or_else(|| std::env::var("HOSTNAME").ok())
        // Windows keeps the computer's name here.
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "My computer".into());
    let name: String = name.trim().chars().take(60).collect();
    let paired = website::pair(&site, &name, &key, |code| {
        println!(
            "\n  On the website, open Settings > My hosts and type this code:\n\n      {code}\n"
        );
        println!("  Waiting for you to type it (10 minutes)...");
    })
    .await?;
    website::save(&options.data, &paired)?;
    println!(
        "Paired with {}. Start hosting with: yuki-vtt-server --tunnel",
        paired.website
    );
    Ok(())
}

async fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    if options.tunnel && tunnel::find(&options.data).is_none() {
        println!("Getting cloudflared for the tunnel...");
        cloudflared::download(&options.data).await?;
    }
    let running = host::start(host::Options {
        data: options.data,
        port: options.port,
        tunnel: options.tunnel,
        origins: options.origins,
    })
    .await?;
    stopped().await;
    running.stop().await;
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
