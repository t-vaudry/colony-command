//! `colony-setup`: the installer's command line. The app's Set up Colony
//! dialog uses the same library; this is what the uninstaller runs, and what
//! works without the app.
//!
//!   colony-setup targets
//!   colony-setup status    [--target T] [--json]
//!   colony-setup install   [--target T] [--no-hooks] [--no-probe] [--no-approval] [--yes] [--dry-run]
//!   colony-setup uninstall [--target T] [--yes] [--dry-run]
//!
//! T is `windows`, `wsl:<distro>` or `all` (default: every running target).
//! `--include-stopped` also starts and reads stopped distros. Test overrides:
//! `--user-home`, `--colony-home`, `--wsl-home`, `--bundle`.

use std::io::{BufRead, Write};

use colony_setup::{apply, plan, status, targets, Options, Selection, TargetInfo};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("colony-setup: {e}");
            2
        }
    });
}

fn run(args: &[String]) -> Result<i32, String> {
    let Some(cmd) = args.first().map(String::as_str) else {
        return Err("usage: colony-setup targets|status|install|uninstall [options]".into());
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let mut opts = Options::from_env();
    if let Some(v) = value("--user-home") {
        opts.user_home = Some(v.into());
    }
    if let Some(v) = value("--colony-home") {
        opts.colony_home = Some(v.into());
    }
    if let Some(v) = value("--wsl-home") {
        opts.wsl_home = Some(v);
    }
    if let Some(v) = value("--bundle") {
        opts.bundle_dirs.insert(0, v.into());
    }
    let json = flag("--json");

    let mut chosen: Vec<TargetInfo> = targets(&opts);
    match value("--target").as_deref() {
        None | Some("all") => {
            if !flag("--include-stopped") {
                chosen.retain(|t| t.running);
            }
        }
        Some(id) => {
            chosen.retain(|t| t.id == id);
            if chosen.is_empty() {
                return Err(format!("no target {id}; try `colony-setup targets`"));
            }
        }
    }

    match cmd {
        "targets" => {
            for t in targets(&opts) {
                if json {
                    println!("{}", serde_json::to_string(&t).unwrap());
                } else {
                    println!("{}\t{}", t.id, if t.running { "running" } else { "stopped" });
                }
            }
            Ok(0)
        }
        "status" => {
            let all: Vec<_> = chosen.iter().map(|t| status(&opts, t)).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&all).unwrap());
            } else {
                for s in &all {
                    println!("{}{}", s.target.label, s.error.as_deref().map(|e| format!("  ERROR: {e}")).unwrap_or_default());
                    if !s.checked && s.error.is_none() {
                        println!("  (stopped, not read)");
                    }
                    for c in &s.components {
                        println!("  {:<14} {:?}", c.label, c.state);
                    }
                    for w in &s.warnings {
                        println!("  warning: {w}");
                    }
                }
            }
            Ok(0)
        }
        "install" | "uninstall" => {
            let sel = if cmd == "uninstall" {
                Selection::NONE
            } else {
                Selection { hooks: !flag("--no-hooks"), probe: !flag("--no-probe"), approval: !flag("--no-approval") }
            };
            let mut failed = false;
            for t in &chosen {
                let p = plan(&opts, &t.id, sel);
                println!("== {}", t.label);
                if let Some(e) = &p.error {
                    println!("  cannot continue: {e}");
                    failed = true;
                    continue;
                }
                if p.nothing {
                    println!("  nothing to do");
                    continue;
                }
                if !p.diff.is_empty() {
                    println!("{}", p.diff);
                }
                for f in &p.files {
                    println!("  {:<7} {}", f.action, f.path);
                }
                if flag("--dry-run") {
                    continue;
                }
                if !flag("--yes") && !confirm(&format!("Apply these changes to {}?", t.label)) {
                    println!("  skipped");
                    continue;
                }
                let done = apply(&opts, &t.id, sel, Some(&p.token));
                for s in &done.steps {
                    println!("  {} {}{}", if s.ok { "ok  " } else { "FAIL" }, s.what, s.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default());
                }
                failed |= !done.ok;
            }
            Ok(i32::from(failed))
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && line.trim().eq_ignore_ascii_case("y")
}
