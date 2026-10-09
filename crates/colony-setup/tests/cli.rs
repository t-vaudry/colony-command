//! The real `colony-setup` binary, run the way the installer and a user run it,
//! against a scratch home. Covers issue #22's acceptance: finishing setup puts
//! Colony's entries in settings.json, and a build without the hook binary fails
//! loudly instead of doing nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch install folder holding a copy of colony-setup (outside any `target`
/// folder, so it can only find what is placed beside it, as in an installed app).
struct Install {
    root: PathBuf,
    exe: PathBuf,
}

impl Install {
    fn new(name: &str) -> Install {
        let root = std::env::temp_dir().join(format!("colony-setup-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let exe = app.join(if cfg!(windows) { "colony-setup.exe" } else { "colony-setup" });
        fs::copy(env!("CARGO_BIN_EXE_colony-setup"), &exe).unwrap();
        Install { root, exe }
    }
    fn stage_hook(&self) {
        fs::write(self.root.join("app/colony-hook.exe"), b"hook").unwrap();
    }
    fn settings(&self) -> PathBuf {
        self.root.join("home/.claude/settings.json")
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(&self.exe)
            .args(args)
            .arg("--target")
            .arg("windows")
            .arg("--user-home")
            .arg(self.root.join("home"))
            .env_remove("COLONY_BUNDLE_DIR")
            .env("COLONY_HOME", self.root.join("home/.colony"))
            .output()
            .unwrap()
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn has(p: &Path) -> bool {
    p.exists()
}

#[test]
fn install_writes_hook_entries_then_uninstall_removes_them() {
    let i = Install::new("ok");
    i.stage_hook();
    fs::create_dir_all(i.settings().parent().unwrap()).unwrap();
    fs::write(i.settings(), "{\"model\":\"opus\"}\n").unwrap();

    let o = i.run(&["install", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(i.settings()).unwrap()).unwrap();
    assert_eq!(v["model"], "opus");
    for ev in ["SessionStart", "PreToolUse", "Stop", "PermissionRequest", "SessionEnd"] {
        let s = v["hooks"][ev].to_string();
        assert!(s.contains("colony-hook") && s.contains("# colony-setup v="), "{ev}: {s}");
    }
    assert!(has(&i.root.join("home/.colony/bin/colony-hook.exe")));

    // Running it again changes nothing.
    assert!(text(&i.run(&["install", "--yes"])).contains("nothing to do"));

    let o = i.run(&["uninstall", "--yes"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(fs::read_to_string(i.settings()).unwrap(), "{\"model\":\"opus\"}\n");
    assert!(!has(&i.root.join("home/.colony/bin/colony-hook.exe")));
}

#[test]
fn install_without_the_hook_binary_fails_loudly_and_writes_nothing() {
    let i = Install::new("missing");
    let o = i.run(&["install", "--yes"]);
    assert!(!o.status.success(), "must exit non-zero: {}", text(&o));
    let t = text(&o);
    assert!(t.contains("colony-hook.exe") && t.contains("Looked in"), "{t}");
    assert!(!has(&i.settings()));
    assert!(!has(&i.root.join("home/.colony")));
}
