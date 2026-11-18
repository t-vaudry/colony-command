//! Which commands a remote device may send. Gated by the command's `type`
//! name before it is even parsed, and deny by default: a name not listed here
//! needs control. The test at the bottom reads `enum Command` in `api.rs` and
//! fails when a variant is missing from the tables below, so a new command
//! can't slip through ungated.

use crate::devices::Scope;

/// Read-only, or harmless for a remote client (its resize is ignored, see `api`).
pub const VIEW: &[&str] = &["attach", "detach", "presence", "resize", "list_workflows", "folder_crew", "check_crew", "replay"];

/// Managing devices: the desktop only, whatever a device was granted.
pub const DESKTOP_ONLY: &[&str] = &["create_pairing", "list_devices", "revoke_device", "set_device_scope", "list_audit"];

/// Everything that changes state or acts for the user: answering approvals,
/// typing into or ending sessions, starting work, editing crews and workflows,
/// changing settings and budgets.
#[cfg_attr(not(test), allow(dead_code))] // the deny-by-default gate never reads it; the tests do
pub const CONTROL: &[&str] = &[
    "ack", "spawn", "draft_workflow", "save_workflow", "delete_workflow", "save_crew", "delete_crew", "set_folder_crew",
    "clear_folder_crew", "input", "send", "interrupt", "kill", "set_model", "reply", "pause", "cancel_pause", "resume",
    "delete_rule", "permission", "terminate", "focus", "dismiss", "rename", "state_feedback", "set_classifier",
    "set_context_pct", "compact", "set_budgets", "raise_budget", "undo_budget", "clock", "open_worktree",
    "discard_worktree", "forget_worktree", "bring_back", "fix_need",
];

/// Why a device's command was refused: a code and a message the client can show.
pub struct Refused {
    pub code: &'static str,
    pub message: &'static str,
}

/// Whether a remote device with `scope` may send a command of this type.
pub fn allow(command_type: &str, scope: Scope) -> Result<(), Refused> {
    if DESKTOP_ONLY.contains(&command_type) {
        return Err(Refused { code: "desktop_only", message: "Only the desktop app can do that." });
    }
    if VIEW.contains(&command_type) || scope == Scope::Control {
        return Ok(());
    }
    Err(Refused { code: "view_only", message: "This device is view-only. Grant it control in the desktop app to do that." })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The variant names of `enum Command`, as the `type` they are sent with.
    fn command_types() -> Vec<String> {
        let src = include_str!("api.rs");
        let body = &src[src.find("\nenum Command {").expect("enum Command") + 1..];
        // "\n}" and not "\n}\n": a Windows checkout has CRLF line endings.
        let body = &body[..body.find("\n}").unwrap()];
        body.lines()
            .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
            .map(str::trim_start)
            .filter(|l| l.chars().next().is_some_and(char::is_uppercase))
            .map(|l| {
                let name: String = l.chars().take_while(|c| c.is_alphanumeric()).collect();
                let mut out = String::new();
                for (i, c) in name.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        out.push('_');
                    }
                    out.push(c.to_ascii_lowercase());
                }
                out
            })
            .collect()
    }

    #[test]
    fn every_command_is_in_exactly_one_table() {
        let types = command_types();
        assert!(types.len() > 40, "found {} variants; is the scan broken?", types.len());
        for t in &types {
            let n = [VIEW, DESKTOP_ONLY, CONTROL].iter().filter(|table| table.contains(&t.as_str())).count();
            assert_eq!(n, 1, "command `{t}` must be in exactly one of VIEW, DESKTOP_ONLY, CONTROL");
        }
        for t in VIEW.iter().chain(DESKTOP_ONLY).chain(CONTROL) {
            assert!(types.iter().any(|c| c == t), "`{t}` is listed but is not a command");
        }
    }

    #[test]
    fn a_view_scoped_device_is_refused_every_command_that_changes_anything() {
        for t in CONTROL.iter().chain(DESKTOP_ONLY) {
            assert!(allow(t, Scope::View).is_err(), "{t} must be refused to a view device");
        }
        assert_eq!(allow("input", Scope::View).err().unwrap().code, "view_only");
        for t in VIEW {
            assert!(allow(t, Scope::View).is_ok(), "{t} is read-only");
        }
    }

    #[test]
    fn control_allows_everything_but_device_management() {
        for t in VIEW.iter().chain(CONTROL) {
            assert!(allow(t, Scope::Control).is_ok(), "{t}");
        }
        for t in DESKTOP_ONLY {
            assert_eq!(allow(t, Scope::Control).err().unwrap().code, "desktop_only", "{t}");
        }
    }

    #[test]
    fn an_unknown_command_needs_control() {
        assert!(allow("brand_new_thing", Scope::View).is_err());
        assert!(allow("brand_new_thing", Scope::Control).is_ok());
    }
}
