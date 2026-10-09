//! OS notifications for items that have waited past their patience budget.
//! The decisions are `colony_core::patience`; this only shows them.

use std::sync::Mutex;

use colony_core::patience::{compose, Item, Notice, Notifier, Settings};
use tauri::{AppHandle, State};
use tauri_plugin_notification::NotificationExt;

#[derive(Default)]
pub struct Patience(Mutex<Notifier>);

/// The map calls this about once a second with the porch's items. Shows a
/// notification for each newly overdue one and returns what it showed.
#[tauri::command]
pub fn patience_tick(app: AppHandle, state: State<Patience>, now: u64, items: Vec<Item>, settings: Settings, attended: bool) -> Vec<Notice> {
    let due = state.0.lock().unwrap_or_else(|e| e.into_inner()).due(now, &items, &settings, attended);
    let notices = compose(&due);
    for n in &notices {
        if let Err(e) = app.notification().builder().title(&n.title).body(&n.body).show() {
            eprintln!("colony: notification failed: {e}");
        }
    }
    notices
}

/// "Send a test notification" in the settings, so the user can see it works.
#[tauri::command]
pub fn patience_test(app: AppHandle) -> Result<(), String> {
    app.notification().builder().title("Colony Command").body("Notifications are on. This is what an overdue bot looks like.").show().map_err(|e| e.to_string())
}
