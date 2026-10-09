//! Automatic updates: the map asks whether a newer release is published on GitHub
//! and, only when the user says so, downloads and installs it. Nothing here runs an
//! install by itself, so a running session is never interrupted by an update.
//!
//! The NSIS installer's hooks stop this install's colonyd and move a running
//! colony-ptyd aside, so sessions survive the upgrade; the app restarts afterwards.

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
