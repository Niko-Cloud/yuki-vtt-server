//! The window and tray for Yuki VTT Server. The DM starts and stops hosting here, pairs the
//! host with their account, and sees their tables. Closing the window keeps it in the tray;
//! the game itself is played on the website.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent, Wry};
use tauri_plugin_opener::OpenerExt;
use yuki_vtt_server::host::{self, Running, TableCard};
use yuki_vtt_server::{cloudflared, secrets, tunnel, website};

const PORT: u16 = 4610;

/// What the app keeps while it runs.
struct Shared {
    data: PathBuf,
    /// The host, while it is on.
    running: tokio::sync::Mutex<Option<Running>>,
    /// What the window shows between clicks: a start under way, a pairing code, a problem.
    notes: Mutex<Notes>,
    /// The tray's lines, to reword as things change. None when the system has no tray.
    tray: Mutex<Option<TrayLines>>,
}

#[derive(Default, Clone)]
struct Notes {
    starting: bool,
    pairing_code: Option<String>,
    pairing: bool,
    problem: Option<String>,
}

struct TrayLines {
    status: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    hosting: bool,
    starting: bool,
    /// Where players reach the host now; none until the tunnel is up.
    address: Option<String>,
    /// The website this host is paired with.
    website: Option<String>,
    pairing: bool,
    pairing_code: Option<String>,
    problem: Option<String>,
    /// Screens with a table open right now.
    screens: usize,
    tables: Vec<TableCard>,
    data_folder: String,
    start_on_open: bool,
    has_tray: bool,
    version: &'static str,
}

fn paired_website(shared: &Shared) -> Option<String> {
    website::load(&shared.data)
        .ok()
        .flatten()
        .map(|(site, _)| site)
}

// One small choice kept between runs: whether hosting starts when the app opens.
fn start_on_open(shared: &Shared) -> bool {
    secrets::read(&shared.data.join("start-on-open"))
        .ok()
        .flatten()
        .as_deref()
        != Some("no")
}

fn note(shared: &Shared, change: impl FnOnce(&mut Notes)) {
    change(&mut shared.notes.lock().unwrap_or_else(|p| p.into_inner()));
}

#[tauri::command]
async fn status(shared: State<'_, Shared>) -> Result<Status, String> {
    let running = shared.running.lock().await;
    let notes = shared
        .notes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    Ok(Status {
        hosting: running.is_some(),
        starting: notes.starting,
        address: running.as_ref().and_then(Running::public_address),
        website: paired_website(&shared),
        pairing: notes.pairing,
        pairing_code: notes.pairing_code,
        problem: notes.problem,
        screens: running
            .as_ref()
            .map(|r| r.state.events.listeners())
            .unwrap_or(0),
        tables: host::tables(&shared.data).unwrap_or_default(),
        data_folder: shared.data.display().to_string(),
        start_on_open: start_on_open(&shared),
        has_tray: shared
            .tray
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Switches hosting on: gets cloudflared the first time, then starts the host with its tunnel.
async fn start(app: &AppHandle) -> Result<(), String> {
    let shared = app.state::<Shared>();
    let mut running = shared.running.lock().await;
    if running.is_some() {
        return Ok(());
    }
    note(&shared, |n| {
        n.starting = true;
        n.problem = None;
    });
    refresh_tray(app, false, None);
    let started = async {
        if tunnel::find(&shared.data).is_none() {
            cloudflared::download(&shared.data).await?;
        }
        host::start(host::Options {
            data: shared.data.clone(),
            port: PORT,
            tunnel: true,
            origins: vec![],
        })
        .await
    }
    .await;
    note(&shared, |n| n.starting = false);
    match started {
        Ok(host) => {
            *running = Some(host);
            refresh_tray(app, true, None);
            Ok(())
        }
        Err(problem) => {
            note(&shared, |n| n.problem = Some(problem.clone()));
            refresh_tray(app, false, None);
            Err(problem)
        }
    }
}

async fn stop(app: &AppHandle) {
    let shared = app.state::<Shared>();
    if let Some(host) = shared.running.lock().await.take() {
        host.stop().await;
    }
    refresh_tray(app, false, None);
}

#[tauri::command]
async fn start_hosting(app: AppHandle) -> Result<(), String> {
    start(&app).await
}

#[tauri::command]
async fn stop_hosting(app: AppHandle) -> Result<(), String> {
    stop(&app).await;
    Ok(())
}

/// Pairs with the DM's account, like a TV signing in. It answers at once; the window then
/// shows the code to type on the website, and the outcome.
#[tauri::command]
async fn pair(app: AppHandle, website_url: String, name: String) -> Result<(), String> {
    let shared = app.state::<Shared>();
    let Some(site) = website::origin_of(&website_url) else {
        let problem = "The website is an address like https://yuki.example".to_string();
        note(&shared, |n| n.problem = Some(problem.clone()));
        return Err(problem);
    };
    let key = host::owner_key(&shared.data)
        .map_err(|error| format!("Couldn't keep the DM's key: {error}"))?;
    let name: String = name.trim().chars().take(60).collect();
    let name = if name.is_empty() {
        "My computer".to_string()
    } else {
        name
    };
    note(&shared, |n| {
        n.pairing = true;
        n.pairing_code = None;
        n.problem = None;
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let shared = app.state::<Shared>();
        let show = app.clone();
        let outcome = website::pair(&site, &name, &key, move |code| {
            note(&show.state::<Shared>(), |n| {
                n.pairing_code = Some(code.to_string())
            });
        })
        .await
        .and_then(|paired| {
            website::save(&shared.data, &paired)
                .map_err(|error| format!("Couldn't keep the pairing: {error}"))
        });
        note(&shared, |n| {
            n.pairing = false;
            n.pairing_code = None;
            n.problem = outcome.as_ref().err().cloned();
        });
        if outcome.is_ok() {
            // A host that is on starts again, to trust the website and tell it where it is.
            let was_on = shared.running.lock().await.is_some();
            if was_on {
                stop(&app).await;
            }
            if was_on || start_on_open(&shared) {
                let _ = start(&app).await;
            }
        }
    });
    Ok(())
}

/// Forgets the website. The tables stay; nobody can reach them until the host is paired again.
#[tauri::command]
async fn unpair(app: AppHandle) -> Result<(), String> {
    stop(&app).await;
    let shared = app.state::<Shared>();
    for file in ["website", "host-key", "website-secret"] {
        let _ = std::fs::remove_file(shared.data.join(file));
    }
    Ok(())
}

#[tauri::command]
fn set_start_on_open(shared: State<'_, Shared>, on: bool) -> Result<(), String> {
    secrets::write(
        &shared.data.join("start-on-open"),
        if on { "yes" } else { "no" },
    )
    .map_err(|error| format!("Couldn't keep that choice: {error}"))
}

#[tauri::command]
fn open_data_folder(app: AppHandle, shared: State<'_, Shared>) -> Result<(), String> {
    app.opener()
        .open_path(shared.data.display().to_string(), None::<&str>)
        .map_err(|error| format!("Couldn't open the folder: {error}"))
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// The tray's two lines: what the host is doing, and the switch.
fn refresh_tray(app: &AppHandle, hosting: bool, address: Option<&str>) {
    let shared = app.state::<Shared>();
    let starting = shared
        .notes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .starting;
    let lines = shared.tray.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lines) = lines.as_ref() else { return };
    let status = match (hosting, starting, address) {
        (_, true, _) => "Starting...",
        (true, _, Some(_)) => "Hosting: on, players can join",
        (true, _, None) => "Hosting: on, opening the tunnel...",
        _ => "Hosting: off",
    };
    let _ = lines.status.set_text(status);
    let _ = lines.toggle.set_text(if hosting {
        "Stop hosting"
    } else {
        "Start hosting"
    });
    let _ = lines.toggle.set_enabled(!starting);
}

fn build_tray(app: &AppHandle) -> tauri::Result<TrayLines> {
    let open = MenuItem::with_id(app, "open", "Open Yuki VTT Server", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Hosting: off", false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start hosting", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let line = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &toggle, &line, &open, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Yuki VTT Server")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_window(app),
            "toggle" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let on = app.state::<Shared>().running.lock().await.is_some();
                    if on {
                        stop(&app).await;
                    } else if start(&app).await.is_err() {
                        // The window says what went wrong.
                        show_window(&app);
                    }
                });
            }
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(TrayLines { status, toggle })
}

// Quitting switches the host off first, so the tunnel never outlives the app.
fn quit_app(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        stop(&app).await;
        app.exit(0);
    });
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let app = tauri::Builder::default()
        // Opening the app again shows the one that is running.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_window(app)
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            status,
            start_hosting,
            stop_hosting,
            pair,
            unpair,
            set_start_on_open,
            open_data_folder
        ])
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            app.manage(Shared {
                data,
                running: tokio::sync::Mutex::new(None),
                notes: Mutex::default(),
                tray: Mutex::new(None),
            });
            let handle = app.handle().clone();
            // Some Linux desktops have no tray (or lack its library, which fails loudly):
            // the app then works from its window alone.
            let tray =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_tray(&handle)));
            match tray {
                Ok(Ok(lines)) => {
                    *handle
                        .state::<Shared>()
                        .tray
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(lines)
                }
                Ok(Err(error)) => tracing::warn!("no tray icon: {error}"),
                Err(_) => tracing::warn!("no tray icon: this system has no tray library"),
            }
            // The system telling the app to stop (a plain kill, shutting down) is a quit too.
            #[cfg(unix)]
            {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{SignalKind, signal};
                    let (Ok(mut terminate), Ok(mut interrupt)) = (
                        signal(SignalKind::terminate()),
                        signal(SignalKind::interrupt()),
                    ) else {
                        return;
                    };
                    tokio::select! {
                        _ = terminate.recv() => {}
                        _ = interrupt.recv() => {}
                    }
                    quit_app(&handle);
                });
            }
            tauri::async_runtime::spawn(async move {
                let shared = handle.state::<Shared>();
                if paired_website(&shared).is_some() && start_on_open(&shared) {
                    let _ = start(&handle).await;
                }
                // The tray follows the tunnel as its address comes and goes.
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let (hosting, address) = {
                        let running = shared.running.lock().await;
                        (
                            running.is_some(),
                            running.as_ref().and_then(Running::public_address),
                        )
                    };
                    refresh_tray(&handle, hosting, address.as_deref());
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the host going in the tray. Without a tray the window
            // is the only way back, so it only minimizes.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let has_tray = window
                    .state::<Shared>()
                    .tray
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_some();
                let _ = if has_tray {
                    window.hide()
                } else {
                    window.minimize()
                };
            }
        })
        .build(tauri::generate_context!())
        .expect("Yuki VTT Server couldn't start");
    app.run(|app, event| {
        // The system asking the app to go (logging out, say) stops the host too.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = &event
        {
            api.prevent_exit();
            quit_app(app);
        }
    });
}
