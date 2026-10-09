//! The approvals kill switch: while `<colony home>/gating-paused` exists,
//! Colony's permission gating steps aside and Claude Code's own prompt decides.
//!
//! The tray's "Pause approvals gating" creates and removes the file. Anything
//! unexpected (no file, unreadable folder) reads as "not paused", so the switch
//! itself can never block a tool call: the gate only ever steps aside.

use std::path::{Path, PathBuf};

pub const MARKER: &str = "gating-paused";

pub fn marker_path(colony_home: &Path) -> PathBuf {
    colony_home.join(MARKER)
}

pub fn paused(colony_home: &Path) -> bool {
    marker_path(colony_home).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_decides() {
        let dir = std::env::temp_dir().join(format!("colony-gating-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!paused(&dir));
        std::fs::write(marker_path(&dir), b"").unwrap();
        assert!(paused(&dir));
        assert!(!paused(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
