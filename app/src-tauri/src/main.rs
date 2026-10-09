// No console window behind the map in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod daemon;
mod gating;
mod tray;

use colony_setup::{Applied, Options, Plan, Selection, TargetStatus};
use serde::Serialize;
use tauri::Manager;

/// The map calls this to connect; starts colonyd if it isn't running.
#[tauri::command]
async fn daemon_info() -> Result<daemon::Info, String> {
    tauri::async_runtime::spawn_blocking(daemon::ensure).await.map_err(|e| e.to_string())?
}

fn setup_options(app: &tauri::AppHandle) -> Options {
    let mut opts = Options::from_env();
    if let Ok(dir) = app.path().resource_dir() {
        opts.bundle_dirs.push(dir);
    }
    opts
}

#[derive(Serialize)]
struct SetupStatus {
    /// The version this app installs.
    version: &'static str,
    targets: Vec<TargetStatus>,
}

/// Windows and every WSL distro, with what Colony has installed in each.
/// Stopped distros are listed but not read (reading would start them).
#[tauri::command]
async fn setup_status(app: tauri::AppHandle, include_stopped: Vec<String>) -> Result<SetupStatus, String> {
    let opts = setup_options(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let targets = colony_setup::targets(&opts)
            .into_iter()
            .map(|mut t| {
                // The user asked for this stopped distro; reading it starts it.
                if include_stopped.contains(&t.id) {
                    t.running = true;
                }
                colony_setup::status(&opts, &t)
            })
            .collect();
        SetupStatus { version: colony_setup::VERSION, targets }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn setup_plan(app: tauri::AppHandle, target: String, selection: Selection) -> Result<Plan, String> {
    let opts = setup_options(&app);
    tauri::async_runtime::spawn_blocking(move || colony_setup::plan(&opts, &target, selection)).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn setup_apply(app: tauri::AppHandle, target: String, selection: Selection, token: String) -> Result<Applied, String> {
    let opts = setup_options(&app);
    tauri::async_runtime::spawn_blocking(move || colony_setup::apply(&opts, &target, selection, Some(&token))).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn autostart_enabled() -> bool {
    autostart::enabled()
}

#[tauri::command]
fn autostart_set(on: bool) -> Result<bool, String> {
    autostart::set(on)?;
    Ok(autostart::enabled())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // The window starts hidden (tauri.conf.json) so a login start can stay
            // in the tray. Without a tray there'd be no way back, so show it then.
            let handle = app.handle();
            let tray_ok = tray::init(handle);
            if tray_ok {
                tray::hide_on_close(handle);
            }
            if !(tray_ok && std::env::args().any(|a| a == "--tray")) {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                }
            }
            // Start the daemon while the window loads rather than on first ask.
            std::thread::spawn(|| {
                if let Err(e) = daemon::ensure() {
                    eprintln!("colony: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![daemon_info, setup_status, setup_plan, setup_apply, autostart_enabled, autostart_set])
        .run(tauri::generate_context!())
        .expect("Colony Command failed to start");
}
