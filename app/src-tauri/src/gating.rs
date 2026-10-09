//! The approvals kill switch, as a marker file under Colony's home.
//!
//! While `~/.colony/gating-paused` (or `$COLONY_HOME/gating-paused`) exists,
//! colony-hook, colony-approve.sh and colonyd all step aside and Claude Code's
//! own permission prompt decides. See `colony_source::gating`, which the hook
//! and daemon use. A missing or unreadable file means gating is on as usual, so
//! the switch itself can never hold up a tool call.

use std::path::PathBuf;

fn marker() -> PathBuf {
    colony_source::gating::marker_path(&crate::daemon::colony_home())
}

pub fn paused() -> bool {
    marker().exists()
}

pub fn set_paused(on: bool) -> Result<(), String> {
    let path = marker();
    if on {
        std::fs::create_dir_all(path.parent().unwrap_or(&path)).map_err(|e| e.to_string())?;
        std::fs::write(&path, b"paused from the Colony tray\n").map_err(|e| e.to_string())
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}
