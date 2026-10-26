//! Automatic updates: the map asks whether a newer release is published on GitHub
//! and, only when the user says so, downloads and installs it. Nothing here runs an
//! install by itself, so a running session is never interrupted by an update.
//!
//! The NSIS installer's hooks stop this install's colonyd, then move every sidecar exe
//! (colonyd, colony-ptyd, colony-hook, colony-setup) aside to `<name>.<tick>.old` before
//! copying, since a running exe can be renamed but not overwritten. colony-ptyd keeps
//! running so sessions survive the upgrade; the app restarts afterwards and deletes the
//! leftovers (`clean_old_sidecars`). If a colonyd.exe on disk is still the old version,
//! daemon.rs says the update didn't finish.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

#[derive(Serialize)]
pub struct Available {
    version: String,
    notes: Option<String>,
}

/// None when up to date, in a development build, or when checking is switched off
/// (COLONY_NO_UPDATE_CHECK set). Errors (offline, no release yet) are for the caller to ignore.
#[tauri::command]
pub async fn update_check(app: AppHandle, state: State<'_, Pending>) -> Result<Option<Available>, String> {
    if cfg!(debug_assertions) || std::env::var_os("COLONY_NO_UPDATE_CHECK").is_some() {
        return Ok(None);
    }
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let found = update.as_ref().map(|u| Available { version: u.version.clone(), notes: u.body.clone() });
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = update;
    Ok(found)
}

/// Downloads and runs the installer found by `update_check`. On Windows the installer
/// takes over and this process exits; the new version starts when it finishes.
#[tauri::command]
pub async fn update_install(state: State<'_, Pending>) -> Result<(), String> {
    // Clone, so a failed download can be retried with the same pending update.
    let update = state.0.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or("no update to install")?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(())
}

/// Best effort, at startup: deletes the renamed-aside sidecars (`colonyd.123.old`, ...) the
/// installer left next to this executable. One that is still running can't be deleted;
/// that failure, like any other, is ignored.
pub fn clean_old_sidecars() {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) else { return };
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if (name.starts_with("colonyd.") || name.starts_with("colony-")) && name.ends_with(".old") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
