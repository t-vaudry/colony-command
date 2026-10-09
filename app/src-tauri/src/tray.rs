//! Tray icon: show/hide the map, how many bots need you, the approvals kill
//! switch, autostart at login, and Quit.
//!
//! Closing the window hides it to the tray; the daemon keeps running either way
//! (see daemon.rs). Only Quit from the tray exits the app, and it still leaves
//! colonyd alone.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};

use crate::{autostart, daemon, gating};

const TRAY_ID: &str = "main";
const POLL: Duration = Duration::from_secs(5);
/// Polls between registry reads for the autostart checkmark.
const AUTOSTART_EVERY: u32 = 12;

/// Bots needing a human, by the same grouping as the map's porch.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Needs {
    /// Blocked or crashed.
    pub critical: usize,
    /// A permission to decide, or a question to answer.
    pub input: usize,
    /// Finished work waiting for review.
    pub review: usize,
}

impl Needs {
    fn total(&self) -> usize {
        self.critical + self.input + self.review
    }

    fn label(&self) -> String {
        if self.total() == 0 {
            return "Nothing needs you".into();
        }
        let mut parts = Vec::new();
        for (n, what) in [(self.critical, "blocked"), (self.input, "waiting on you"), (self.review, "to review")] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        parts.join(" · ")
    }
}

/// Counts main agents (not subagents) by what they need, from `/api/agents`.
pub fn count_needs(snapshot: &Value) -> Needs {
    let mut n = Needs::default();
    let agents = snapshot.get("agents").and_then(Value::as_array).or_else(|| snapshot.as_array());
    for a in agents.into_iter().flatten() {
        if a.get("kind").and_then(Value::as_str) != Some("main") {
            continue;
        }
        match a.get("state").and_then(Value::as_str) {
            Some("blocked" | "crashed") => n.critical += 1,
            Some("needs_input" | "awaiting_reply") => n.input += 1,
            Some("ready_to_review") => n.review += 1,
            _ => {}
        }
    }
    n
}

/// One plain-HTTP read of the daemon's snapshot; `None` if it isn't answering.
fn fetch_needs() -> Option<Needs> {
    let info: daemon::Info = serde_json::from_slice(&std::fs::read(daemon::colony_home().join("daemon.json")).ok()?).ok()?;
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let req = format!("GET /api/agents?token={} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n", info.token);
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    if !text.starts_with("HTTP/1.1 200") {
        return None;
    }
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    Some(count_needs(&serde_json::from_str(text.get(start..=end)?).ok()?))
}

pub fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(w) if w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) => {
            let _ = w.hide();
        }
        _ => show(app),
    }
}

/// Builds the tray. Returns false if it couldn't (no tray on this desktop), in
/// which case closing the window must really close it.
pub fn init(app: &AppHandle) -> bool {
    match build(app) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("colony: tray unavailable: {e}");
            false
        }
    }
}

fn build(app: &AppHandle) -> tauri::Result<()> {
    let toggle_item = MenuItem::with_id(app, "toggle", "Show / Hide window", true, None::<&str>)?;
    let needs_item = MenuItem::with_id(app, "needs", Needs::default().label(), false, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause approvals gating", true, gating::paused(), None::<&str>)?;
    let auto = CheckMenuItem::with_id(app, "autostart", "Start at login", true, autostart::enabled(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let (sep1, sep2) = (PredefinedMenuItem::separator(app)?, PredefinedMenuItem::separator(app)?);
    let menu = Menu::with_items(app, &[&toggle_item, &needs_item, &sep1, &pause, &auto, &sep2, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Colony Command")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event({
            let (pause, auto) = (pause.clone(), auto.clone());
            move |app, ev| match ev.id().as_ref() {
                "toggle" => toggle(app),
                "pause" => {
                    // The check flips before the event; the file decides what is true.
                    let want = pause.is_checked().unwrap_or(false);
                    if let Err(e) = gating::set_paused(want) {
                        eprintln!("colony: can't change approvals gating: {e}");
                    }
                    let _ = pause.set_checked(gating::paused());
                }
                "autostart" => {
                    let want = auto.is_checked().unwrap_or(false);
                    if let Err(e) = autostart::set(want) {
                        eprintln!("colony: can't change autostart: {e}");
                    }
                    let _ = auto.set_checked(autostart::enabled());
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                toggle(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;

    // Keep the counts, the tooltip and the checkmarks true to the world: the
    // file can also change from the map's Set up window or by hand.
    let app = app.clone();
    let mut tick = 0u32;
    std::thread::spawn(move || loop {
        // The registry read spawns reg.exe, so it runs about once a minute.
        if tick % AUTOSTART_EVERY == 0 {
            let _ = auto.set_checked(autostart::enabled());
        }
        tick = tick.wrapping_add(1);
        let needs = fetch_needs();
        let label = needs.map(|n| n.label()).unwrap_or_else(|| "Colony isn't running".into());
        let _ = needs_item.set_text(&label);
        let _ = pause.set_checked(gating::paused());
        if let Some(t) = app.tray_by_id(TRAY_ID) {
            let paused = if gating::paused() { " · approvals paused" } else { "" };
            let _ = t.set_tooltip(Some(format!("Colony Command · {label}{paused}")));
        }
        std::thread::sleep(POLL);
    });
    Ok(())
}

/// Closing the window hides it; the tray brings it back.
pub fn hide_on_close(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let win = w.clone();
        w.on_window_event(move |ev| {
            if let WindowEvent::CloseRequested { api, .. } = ev {
                api.prevent_close();
                let _ = win.hide();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_main_agents_by_need() {
        let snap = json!({ "agents": [
            { "kind": "main", "state": "blocked" },
            { "kind": "main", "state": "crashed" },
            { "kind": "main", "state": "needs_input" },
            { "kind": "main", "state": "awaiting_reply" },
            { "kind": "main", "state": "ready_to_review" },
            { "kind": "main", "state": "working" },
            { "kind": "subagent", "state": "needs_input" },
        ]});
        let n = count_needs(&snap);
        assert_eq!(n, Needs { critical: 2, input: 2, review: 1 });
        assert_eq!(n.label(), "2 blocked · 2 waiting on you · 1 to review");
        assert_eq!(count_needs(&json!({})).label(), "Nothing needs you");
    }
}
