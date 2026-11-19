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
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
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
    /// The website asked, by a link, to pair this computer: its address and the link's token,
    /// until the DM says yes or no in the window.
    link_offer: Option<(String, String)>,
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
    /// A website asking to pair this computer, waiting for a yes.
    link_offer: Option<String>,
    /// Screens with a table open right now.
    screens: usize,
    /// Players waiting for the DM to let them in, at every table.
    requests: Vec<serde_json::Value>,
    tables: Vec<TableCard>,
    data_folder: String,
    /// Files in the picture library folder the host couldn't take, with why.
    library_skipped: Vec<String>,
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
        link_offer: notes.link_offer.map(|(site, _)| site),
        screens: running
            .as_ref()
            .map(|r| r.state.events.listeners())
            .unwrap_or(0),
        requests: running.as_ref().map(waiting).unwrap_or_default(),
        tables: host::tables(&shared.data).unwrap_or_default(),
        data_folder: shared.data.display().to_string(),
        library_skipped: running
            .as_ref()
            .map(|r| {
                r.state
                    .skipped
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            })
            .unwrap_or_default(),
        start_on_open: start_on_open(&shared),
        has_tray: shared
            .tray
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some(),
        version: env!("CARGO_PKG_VERSION"),
    })
}

fn waiting(running: &Running) -> Vec<serde_json::Value> {
    yuki_vtt_server::requests::all_waiting(&running.state.db()).unwrap_or_default()
}

/// The DM lets a waiting player in, or turns them away.
#[tauri::command]
async fn answer_request(
    app: AppHandle,
    table_id: String,
    id: String,
    accept: bool,
) -> Result<(), String> {
    let shared = app.state::<Shared>();
    let running = shared.running.lock().await;
    let Some(host) = running.as_ref() else {
        return Err("Start hosting to answer: the player is waiting on this computer".into());
    };
    yuki_vtt_server::requests::answer(&host.state, &table_id, &id, accept)
        .map_err(|error| error.message)?;
    Ok(())
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
    refresh_tray(app, false, None, 0);
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
            refresh_tray(app, true, None, 0);
            Ok(())
        }
        Err(problem) => {
            note(&shared, |n| n.problem = Some(problem.clone()));
            refresh_tray(app, false, None, 0);
            Err(problem)
        }
    }
}

async fn stop(app: &AppHandle) {
    let shared = app.state::<Shared>();
    if let Some(host) = shared.running.lock().await.take() {
        host.stop().await;
    }
    refresh_tray(app, false, None, 0);
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
        let problem = "The website is an https address like https://yuki.example".to_string();
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
        let show = app.clone();
        let outcome = website::pair(&site, &name, &key, move |code| {
            note(&show.state::<Shared>(), |n| {
                n.pairing_code = Some(code.to_string())
            });
        })
        .await;
        paired(&app, outcome).await;
    });
    Ok(())
}

// After pairing, by code or by link: keeps it, and starts hosting so the website sees the
// computer at once. A host that was on starts again, to trust the website.
async fn paired(app: &AppHandle, outcome: Result<website::Paired, String>) {
    let shared = app.state::<Shared>();
    let outcome = outcome.and_then(|paired| {
        website::save(&shared.data, &paired)
            .map_err(|error| format!("Couldn't keep the pairing: {error}"))
    });
    note(&shared, |n| {
        n.pairing = false;
        n.pairing_code = None;
        n.problem = outcome.as_ref().err().cloned();
    });
    if outcome.is_ok() {
        stop(app).await;
        let _ = start(app).await;
    }
}

fn computer_name() -> String {
    let name = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "My computer".into());
    name.trim().chars().take(60).collect()
}

/// What a `yukivtt://` link asks for, from the website's buttons. Any page could open such a
/// link, so the app asks the DM first: pairing in the window, starting in a small dialog.
fn handle_link(app: &AppHandle, url: &str) {
    let Some(link) = website::read_link(url) else {
        tracing::warn!("ignored a link the app doesn't know: {url}");
        return;
    };
    // The pairing token is a secret: only what the link is for goes in the log.
    match &link {
        website::Link::Start => tracing::info!("opened by a link to start hosting"),
        website::Link::Pair { website, .. } => {
            tracing::info!("opened by a link to pair with {website}")
        }
    }
    let shared = app.state::<Shared>();
    match link {
        website::Link::Start => {
            if paired_website(&shared).is_none() {
                note(&shared, |n| {
                    n.problem = Some("Connect this computer to your account first: Settings > My hosts on the website".into())
                });
                show_window(app);
                return;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if app.state::<Shared>().running.lock().await.is_some() {
                    return;
                }
                let ask = app
                    .dialog()
                    .message("A website asks to start hosting your tables on this computer.")
                    .title("Start hosting?")
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "Start hosting".into(),
                        "Not now".into(),
                    ));
                let yes = tauri::async_runtime::spawn_blocking(move || ask.blocking_show())
                    .await
                    .unwrap_or(false);
                if yes && start(&app).await.is_err() {
                    show_window(&app);
                }
            });
        }
        website::Link::Pair { website, token } => {
            note(&shared, |n| {
                n.link_offer = Some((website, token));
                n.problem = None;
            });
            show_window(app);
        }
    }
}

/// The DM's answer to a website asking, by a link, to pair this computer.
#[tauri::command]
async fn answer_link(app: AppHandle, accept: bool) -> Result<(), String> {
    let shared = app.state::<Shared>();
    let offer = shared
        .notes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .link_offer
        .take();
    let Some((site, token)) = offer else {
        return Ok(());
    };
    if !accept {
        return Ok(());
    }
    let key = host::owner_key(&shared.data)
        .map_err(|error| format!("Couldn't keep the DM's key: {error}"))?;
    note(&shared, |n| {
        n.pairing = true;
        n.problem = None;
    });
    let outcome = website::pair_with_link(&site, &token, &computer_name(), &key).await;
    paired(&app, outcome).await;
    Ok(())
}

/// Saves one table in a file the DM picks.
#[tauri::command]
async fn backup_table(app: AppHandle, id: String, name: String) -> Result<Option<String>, String> {
    let data = app.state::<Shared>().data.clone();
    let suggested: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let dialog = app
        .dialog()
        .file()
        .add_filter("Table backup", &["yukitable"])
        .set_file_name(format!("{}.yukitable", suggested.trim()));
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };
    let saved = path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        yuki_vtt_server::backup::export(&data, &id, &saved)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(path.display().to_string()))
}

/// Brings a table back from a backup file. A table already here is replaced only after the DM
/// says so.
#[tauri::command]
async fn restore_table(app: AppHandle) -> Result<Option<String>, String> {
    let data = app.state::<Shared>().data.clone();
    let dialog = app
        .dialog()
        .file()
        .add_filter("Table backup", &["yukitable"]);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_file())
        .await
        .map_err(|e| e.to_string())?;
    let Some(path) = picked.and_then(|file| file.into_path().ok()) else {
        return Ok(None);
    };
    let attempt = |replace: bool| {
        let (data, path) = (data.clone(), path.clone());
        tauri::async_runtime::spawn_blocking(move || {
            yuki_vtt_server::backup::restore(&data, &path, replace)
        })
    };
    match attempt(false).await.map_err(|e| e.to_string())? {
        Ok((_, name)) => Ok(Some(name)),
        Err(problem) if problem.ends_with("is already on this computer") => {
            let ask = app
                .dialog()
                .message(format!(
                    "{problem}. Put the backup in its place? What the table has now is lost."
                ))
                .title("Replace the table?")
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Replace".into(),
                    "Keep it".into(),
                ));
            let replace = tauri::async_runtime::spawn_blocking(move || ask.blocking_show())
                .await
                .map_err(|e| e.to_string())?;
            if !replace {
                return Ok(None);
            }
            attempt(true)
                .await
                .map_err(|e| e.to_string())?
                .map(|(_, name)| Some(name))
        }
        Err(problem) => Err(problem),
    }
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

/// The copies the host keeps of each table, every twenty minutes while it changes.
#[tauri::command]
fn open_backups_folder(app: AppHandle, shared: State<'_, Shared>) -> Result<(), String> {
    let copies = yuki_vtt_server::backup::copies_folder(&shared.data);
    std::fs::create_dir_all(&copies)
        .map_err(|error| format!("Couldn't make the backups folder: {error}"))?;
    app.opener()
        .open_path(copies.display().to_string(), None::<&str>)
        .map_err(|error| format!("Couldn't open the folder: {error}"))
}

/// The picture library, as folders: Tokens, Props, Handouts, Music and Maps.
#[tauri::command]
fn open_library_folder(app: AppHandle, shared: State<'_, Shared>) -> Result<(), String> {
    let library = yuki_vtt_server::library::root(&shared.data);
    std::fs::create_dir_all(&library)
        .map_err(|error| format!("Couldn't make the picture library folder: {error}"))?;
    app.opener()
        .open_path(library.display().to_string(), None::<&str>)
        .map_err(|error| format!("Couldn't open the folder: {error}"))
}

#[tauri::command]
fn open_data_folder(app: AppHandle, shared: State<'_, Shared>) -> Result<(), String> {
    app.opener()
        .open_path(shared.data.display().to_string(), None::<&str>)
        .map_err(|error| format!("Couldn't open the folder: {error}"))
}

/// The link entry the deep-link plugin writes quotes the program's path, which the desktop
/// spec allows but `xdg-open` can't read outside GNOME and KDE (it then hands the link to the
/// browser). Chrome opens links through `xdg-open`, so the entry gets a plain path: the program
/// itself, or a shortcut to it in the data folder when its own path has spaces (an AppImage
/// kept as downloaded).
#[cfg(target_os = "linux")]
fn plain_link_entry(data: &std::path::Path) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let entry = std::path::PathBuf::from(home)
        .join(".local/share/applications/yuki-vtt-server-desktop-handler.desktop");
    let Ok(text) = std::fs::read_to_string(&entry) else {
        return;
    };
    // An AppImage runs from a temporary mount; the file itself is what to start next time.
    let program = std::env::var_os("APPIMAGE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(program) = program else { return };
    let program = if program.to_string_lossy().contains(char::is_whitespace) {
        let shortcut = data.join("yuki-vtt-server-desktop");
        let _ = std::fs::remove_file(&shortcut);
        if std::os::unix::fs::symlink(&program, &shortcut).is_err()
            || shortcut.to_string_lossy().contains(char::is_whitespace)
        {
            return;
        }
        shortcut
    } else {
        program
    };
    let plain = format!("Exec={} %u", program.display());
    let fixed: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("Exec=") {
                plain.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    let fixed = fixed.join("\n") + "\n";
    if fixed != text {
        let _ = std::fs::write(&entry, fixed);
    }
}

fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// The tray's two lines: what the host is doing, and the switch.
fn refresh_tray(app: &AppHandle, hosting: bool, address: Option<&str>, waiting: usize) {
    let shared = app.state::<Shared>();
    let starting = shared
        .notes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .starting;
    let lines = shared.tray.lock().unwrap_or_else(|p| p.into_inner());
    let Some(lines) = lines.as_ref() else { return };
    let status = match (hosting, starting, address) {
        (_, true, _) => "Starting...".to_string(),
        (true, _, _) if waiting == 1 => "Hosting: on, 1 player waiting to join".to_string(),
        (true, _, _) if waiting > 1 => format!("Hosting: on, {waiting} players waiting to join"),
        (true, _, Some(_)) => "Hosting: on, players can join".to_string(),
        (true, _, None) => "Hosting: on, opening the tunnel...".to_string(),
        _ => "Hosting: off".to_string(),
    };
    let _ = lines.status.set_text(status);
    let _ = lines.toggle.set_text(if hosting {
        "Stop hosting"
    } else {
        "Start hosting"
    });
    let _ = lines.toggle.set_enabled(!starting);
}

// Someone opened the invite link: the DM hears about it even with the window shut.
fn notify_request(app: &AppHandle, request: &serde_json::Value) {
    use tauri_plugin_notification::NotificationExt;
    let who = request["name"].as_str().unwrap_or("Someone");
    let classes = request["classes"].as_str().filter(|c| !c.is_empty());
    let table = request["table"].as_str().unwrap_or("your table");
    let character = classes.map(|c| format!(" ({c})")).unwrap_or_default();
    let shown = app
        .notification()
        .builder()
        .title(format!("{who} wants to join {table}"))
        .body(format!(
            "{who}{character} is waiting. Open Yuki VTT Server, or your DM page, to let them in."
        ))
        .show();
    if let Err(error) = shown {
        tracing::warn!("couldn't show a notification: {error}");
    }
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
        // Opening the app again shows the one that is running; a yukivtt:// link opened while
        // it runs reaches it the same way (and the deep-link plugin hands it over).
        .plugin(tauri_plugin_single_instance::init(|app, argv, _| {
            if !argv.iter().any(|arg| arg.starts_with("yukivtt://")) {
                show_window(app)
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            status,
            start_hosting,
            stop_hosting,
            pair,
            unpair,
            set_start_on_open,
            open_data_folder,
            open_library_folder,
            open_backups_folder,
            answer_link,
            answer_request,
            backup_table,
            restore_table
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
            // The website's buttons open yukivtt:// links. Installers register the scheme;
            // registering here too covers the AppImage and running from the source.
            #[cfg(any(target_os = "linux", windows))]
            if let Err(error) = app.deep_link().register_all() {
                tracing::warn!("couldn't register yukivtt:// links: {error}");
            }
            #[cfg(target_os = "linux")]
            if let Ok(data) = app.path().app_data_dir() {
                plain_link_entry(&data);
            }
            {
                let links = handle.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        handle_link(&links, url.as_str());
                    }
                });
            }
            // The app may have been started by a link.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    handle_link(&handle, url.as_str());
                }
            }
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
                // The tray follows the tunnel as its address comes and goes, and says when
                // someone waits to join; each new one also gets a notification.
                let mut told: std::collections::HashSet<String> = Default::default();
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let (hosting, address, requests) = {
                        let running = shared.running.lock().await;
                        (
                            running.is_some(),
                            running.as_ref().and_then(Running::public_address),
                            running.as_ref().map(waiting).unwrap_or_default(),
                        )
                    };
                    refresh_tray(&handle, hosting, address.as_deref(), requests.len());
                    for request in &requests {
                        let id = request["id"].as_str().unwrap_or_default().to_string();
                        if told.insert(id) {
                            notify_request(&handle, request);
                        }
                    }
                    told.retain(|id| requests.iter().any(|r| r["id"] == id.as_str()));
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
