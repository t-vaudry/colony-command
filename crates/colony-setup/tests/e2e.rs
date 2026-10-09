//! Install, status and uninstall against a scratch home, with the user's own
//! settings next to Colony's.

use std::fs;
use std::path::{Path, PathBuf};

use colony_setup::{apply, plan, status, targets, Options, Selection, State};

struct Scratch {
    root: PathBuf,
    opts: Options,
}

fn scratch(name: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("colony-setup-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (home, bundle) = (root.join("home"), root.join("bundle"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("colony-hook.exe"), b"hook-binary-v1").unwrap();
    fs::write(bundle.join("colony-setup.exe"), b"setup-binary-v1").unwrap();
    let opts = Options { user_home: Some(home.clone()), colony_home: None, wsl_home: None, bundle_dirs: vec![bundle] };
    Scratch { root, opts }
}

impl Scratch {
    fn settings(&self) -> PathBuf {
        self.root.join("home/.claude/settings.json")
    }
    fn bin(&self, f: &str) -> PathBuf {
        self.root.join("home/.colony/bin").join(f)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const USER: &str = "{\r\n  \"model\": \"opus\",\r\n  \"hooks\": {\r\n    \"Stop\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"mine\" } ] } ]\r\n  }\r\n}\r\n";

fn backups(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().contains("colony-backup")).collect()
}

#[test]
fn install_status_uninstall_keeps_the_users_settings() {
    let s = scratch("e2e");
    fs::create_dir_all(s.settings().parent().unwrap()).unwrap();
    fs::write(s.settings(), USER).unwrap();
    let windows = targets(&s.opts).into_iter().find(|t| t.id == "windows").unwrap();

    let before = status(&s.opts, &windows);
    assert!(before.checked, "{:?}", before.error);
    assert!(before.components.iter().all(|c| c.state == State::Missing));

    let p = plan(&s.opts, "windows", Selection::ALL);
    assert!(p.error.is_none(), "{:?}", p.error);
    assert!(p.diff.contains("+") && p.diff.contains("colony-hook"));
    assert!(p.backup_path.is_some());
    // Nothing is written by planning.
    assert_eq!(fs::read_to_string(s.settings()).unwrap(), USER);
    assert!(!s.bin("colony-hook.exe").exists());

    let done = apply(&s.opts, "windows", Selection::ALL, Some(&p.token));
    assert!(done.ok, "{:?}", done.steps);
    assert_eq!(fs::read(s.bin("colony-hook.exe")).unwrap(), b"hook-binary-v1");
    assert!(s.bin("colony-setup.exe").exists());
    let b = backups(s.settings().parent().unwrap());
    assert_eq!(b.len(), 1);
    assert_eq!(fs::read_to_string(&b[0]).unwrap(), USER, "the backup is the original, byte for byte");
    let installed = fs::read_to_string(s.settings()).unwrap();
    assert!(installed.contains("\"command\": \"mine\""));
    assert!(installed.contains("\"model\": \"opus\""));
    assert!(installed.contains("\"timeout\": 600"), "approval timeout on PermissionRequest");

    let after = status(&s.opts, &windows);
    assert!(after.components.iter().all(|c| c.state == State::Current), "{:?}", after.components);

    // Idempotent: a second plan has nothing to do.
    assert!(plan(&s.opts, "windows", Selection::ALL).nothing);

    // A newer bundled binary at the same version shows as changed; an older install as outdated.
    fs::write(s.root.join("bundle/colony-hook.exe"), b"hook-binary-v2").unwrap();
    let st = status(&s.opts, &windows);
    assert_eq!(st.components[0].state, State::Changed);
    fs::write(s.bin("VERSION"), "0.0.1\n").unwrap();
    assert_eq!(status(&s.opts, &windows).components[0].state, State::Outdated);
    let up = apply(&s.opts, "windows", Selection::ALL, None);
    assert!(up.ok);
    assert_eq!(fs::read(s.bin("colony-hook.exe")).unwrap(), b"hook-binary-v2");

    // Uninstall: settings back to the original bytes, our files gone.
    let un = apply(&s.opts, "windows", Selection::NONE, None);
    assert!(un.ok, "{:?}", un.steps);
    assert_eq!(fs::read_to_string(s.settings()).unwrap(), USER);
    assert!(!s.bin("colony-hook.exe").exists());
    // The running uninstaller can't delete itself; the installer removes it afterwards.
    assert!(s.bin("colony-setup.exe").exists());
    assert!(!s.bin("VERSION").exists());
    let end = status(&s.opts, &windows);
    assert!(end.components.iter().all(|c| c.state == State::Missing));
}

#[test]
fn a_plan_that_went_stale_is_refused() {
    let s = scratch("stale");
    let p = plan(&s.opts, "windows", Selection::ALL);
    fs::create_dir_all(s.settings().parent().unwrap()).unwrap();
    fs::write(s.settings(), "{\"model\":\"x\"}").unwrap();
    let done = apply(&s.opts, "windows", Selection::ALL, Some(&p.token));
    assert!(!done.ok);
    assert_eq!(fs::read_to_string(s.settings()).unwrap(), "{\"model\":\"x\"}");
    assert!(!s.bin("colony-hook.exe").exists());
}

#[test]
fn invalid_settings_are_refused_and_untouched() {
    let s = scratch("invalid");
    fs::create_dir_all(s.settings().parent().unwrap()).unwrap();
    fs::write(s.settings(), "{ // my notes\n \"model\": \"x\" }").unwrap();
    let p = plan(&s.opts, "windows", Selection::ALL);
    assert!(p.error.as_deref().is_some_and(|e| e.contains("isn't valid JSON")));
    let done = apply(&s.opts, "windows", Selection::ALL, None);
    assert!(!done.ok);
    assert_eq!(fs::read_to_string(s.settings()).unwrap(), "{ // my notes\n \"model\": \"x\" }");
    assert!(!s.bin("colony-hook.exe").exists());
    assert!(backups(s.settings().parent().unwrap()).is_empty());
    let windows = targets(&s.opts).into_iter().find(|t| t.id == "windows").unwrap();
    assert!(status(&s.opts, &windows).error.is_some());
}

#[test]
fn no_settings_file_is_created_with_no_backup() {
    let s = scratch("fresh");
    let done = apply(&s.opts, "windows", Selection::ALL, None);
    assert!(done.ok, "{:?}", done.steps);
    assert!(fs::read_to_string(s.settings()).unwrap().contains("colony-hook"));
    assert!(backups(s.settings().parent().unwrap()).is_empty());
}

#[test]
fn hooks_without_approval_use_the_short_timeout() {
    let s = scratch("noapproval");
    let sel = Selection { hooks: true, probe: false, approval: false };
    assert!(apply(&s.opts, "windows", sel, None).ok);
    assert!(!fs::read_to_string(s.settings()).unwrap().contains("600"));
    // Turning approval on changes just that entry.
    let p = plan(&s.opts, "windows", Selection::ALL);
    assert_eq!(p.changes.len(), 1);
}

#[test]
fn a_missing_binary_fails_the_plan_before_anything_is_written() {
    let s = scratch("nobin");
    fs::remove_file(s.root.join("bundle/colony-hook.exe")).unwrap();
    let p = plan(&s.opts, "windows", Selection::ALL);
    assert!(p.error.as_deref().is_some_and(|e| e.contains("colony-hook")));
    assert!(!apply(&s.opts, "windows", Selection::ALL, None).ok);
    assert!(!s.settings().exists());
}

#[test]
fn custom_colony_home_is_named_absolutely_in_commands() {
    let mut s = scratch("custom");
    s.opts.colony_home = Some(s.root.join("elsewhere"));
    assert!(apply(&s.opts, "windows", Selection::ALL, None).ok);
    let text = fs::read_to_string(s.settings()).unwrap();
    assert!(text.contains("elsewhere/bin/colony-hook.exe"), "{text}");
    assert!(s.root.join("elsewhere/bin/colony-hook.exe").exists());
}

#[cfg(unix)]
#[test]
fn symlinked_settings_are_written_through_and_modes_kept() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let s = scratch("symlink");
    let real = s.root.join("dotfiles/claude-settings.json");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, "{\"model\":\"x\"}\n").unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir_all(s.settings().parent().unwrap()).unwrap();
    symlink(&real, s.settings()).unwrap();

    assert!(apply(&s.opts, "windows", Selection::ALL, None).ok);
    assert!(fs::symlink_metadata(s.settings()).unwrap().file_type().is_symlink(), "the link was replaced by a file");
    assert!(fs::read_to_string(&real).unwrap().contains("colony-hook"));
    assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
    for b in backups(s.settings().parent().unwrap()) {
        assert_eq!(fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o600, "backup is wider than the original");
    }
    assert!(apply(&s.opts, "windows", Selection::NONE, None).ok);
    assert_eq!(fs::read_to_string(&real).unwrap(), "{\"model\":\"x\"}\n");
    assert!(fs::symlink_metadata(s.settings()).unwrap().file_type().is_symlink());
}

#[test]
fn a_missing_binary_is_named_and_reported_not_silent() {
    let s = scratch("nobin-msg");
    fs::remove_file(s.root.join("bundle/colony-hook.exe")).unwrap();
    let windows = targets(&s.opts).into_iter().find(|t| t.id == "windows").unwrap();
    let st = status(&s.opts, &windows);
    assert_eq!(st.missing_files, vec!["colony-hook.exe".to_string()]);
    assert!(st.warnings.iter().any(|w| w.contains("colony-hook.exe") && w.contains("Looked in")));
    let p = plan(&s.opts, "windows", Selection::ALL);
    let e = p.error.expect("plan must fail loudly");
    assert!(e.contains("colony-hook.exe") && e.contains("cargo build"), "{e}");
    let a = apply(&s.opts, "windows", Selection::ALL, None);
    assert!(!a.ok && a.steps.iter().any(|st| !st.ok && st.detail.as_deref().is_some_and(|d| d.contains("colony-hook.exe"))));
}

#[test]
fn applying_writes_a_hook_entry_for_every_event() {
    let s = scratch("hooks-written");
    let a = apply(&s.opts, "windows", Selection::ALL, None);
    assert!(a.ok, "{:?}", a.steps);
    let text = fs::read_to_string(s.settings()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = v["hooks"].as_object().expect("hooks object written");
    for ev in ["SessionStart", "PreToolUse", "PostToolUse", "Stop", "PermissionRequest", "SessionEnd"] {
        let cmds = hooks[ev].to_string();
        assert!(cmds.contains("colony-hook.exe") && cmds.contains("colony-setup"), "{ev}: {cmds}");
    }
    assert_eq!(fs::read(s.bin("colony-hook.exe")).unwrap(), b"hook-binary-v1");
}

#[test]
fn a_changed_hook_binary_changes_the_fingerprint_and_status() {
    let s = scratch("fingerprint");
    assert!(apply(&s.opts, "windows", Selection::ALL, None).ok);
    let before = colony_setup::bundle_fingerprint(&s.opts);
    fs::write(s.root.join("bundle/colony-hook.exe"), b"hook-binary-v2").unwrap();
    assert_ne!(before, colony_setup::bundle_fingerprint(&s.opts));
    let windows = targets(&s.opts).into_iter().find(|t| t.id == "windows").unwrap();
    let st = status(&s.opts, &windows);
    assert!(st.components.iter().any(|c| c.state != State::Current));
}
