//! Model awareness: which model a session runs, what kind of work it's doing
//! lately, and a suggestion when another model would suit that work better.
//!
//! Suggestions are hints only; the user decides. Rules are deliberately few
//! and conservative, and need a run of recent tool calls before saying
//! anything.

use serde::{Deserialize, Serialize};

/// Recent tool calls kept per agent.
pub const RECENT_TOOLS: usize = 20;
/// No suggestion before this many recent tool calls.
const MIN_SAMPLE: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reading, searching, fetching.
    Read,
    /// Changing files.
    Edit,
    /// Running commands.
    Run,
    /// Planning: todo lists, plan mode.
    Plan,
    Other,
}

pub fn tool_kind(tool: &str) -> ToolKind {
    match tool {
        "Read" | "Grep" | "Glob" | "LS" | "WebFetch" | "WebSearch" | "NotebookRead" => ToolKind::Read,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ToolKind::Edit,
        "Bash" | "PowerShell" | "BashOutput" => ToolKind::Run,
        "TodoWrite" | "EnterPlanMode" | "ExitPlanMode" => ToolKind::Plan,
        _ => ToolKind::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Opus,
    Sonnet,
    Haiku,
    Fable,
}

/// The model family from an id or alias ("claude-opus-5-5", "opus", "Opus 5.5").
pub fn family(model: &str) -> Option<Family> {
    let m = model.to_ascii_lowercase();
    [("opus", Family::Opus), ("sonnet", Family::Sonnet), ("haiku", Family::Haiku), ("fable", Family::Fable)]
        .into_iter()
        .find(|(k, _)| m.contains(k))
        .map(|(_, f)| f)
}

/// A suggested switch, shown on the map with a one-click button.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHint {
    /// What to pass to `/model` or `--model`.
    pub model: String,
    /// Short name for the button, e.g. "Haiku".
    pub label: String,
    pub reason: String,
}

fn hint(model: &str, label: &str, reason: String) -> Option<ModelHint> {
    Some(ModelHint { model: model.into(), label: label.into(), reason })
}

/// Suggest a better-suited model for the work `recent` shows, if any.
pub fn suggest(current: Option<&str>, recent: &[ToolKind]) -> Option<ModelHint> {
    let fam = family(current?)?;
    let n = recent.len();
    if n < MIN_SAMPLE {
        return None;
    }
    let count = |k: ToolKind| recent.iter().filter(|&&t| t == k).count();
    let (reads, edits, runs, plans) = (count(ToolKind::Read), count(ToolKind::Edit), count(ToolKind::Run), count(ToolKind::Plan));

    // Planning a larger piece of work on a lighter model.
    if plans >= 2 && matches!(fam, Family::Haiku | Family::Sonnet) {
        return hint("opus", "Opus", format!("Planning multi-step work ({plans} planning steps in the last {n} tool calls). Opus plans more reliably."));
    }
    // Only reading and searching on a big model.
    if edits == 0 && reads * 10 >= n * 8 && matches!(fam, Family::Opus | Family::Fable | Family::Sonnet) {
        return hint("haiku", "Haiku", format!("Mostly reading and searching ({reads} of the last {n} tool calls, no edits). Haiku is faster and cheaper for that."));
    }
    // Changing and running code on the lightest model.
    if (edits + runs) * 2 >= n && edits > 0 && fam == Family::Haiku {
        return hint("sonnet", "Sonnet", format!("Editing and running code ({} of the last {n} tool calls). Sonnet makes changes more reliably than Haiku.", edits + runs));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToolKind::*;

    fn many(k: ToolKind, n: usize) -> Vec<ToolKind> {
        vec![k; n]
    }

    #[test]
    fn families() {
        assert_eq!(family("claude-opus-5-5"), Some(Family::Opus));
        assert_eq!(family("Haiku 5.5"), Some(Family::Haiku));
        assert_eq!(family("something-else"), None);
    }

    #[test]
    fn reading_on_opus_suggests_haiku() {
        let mut r = many(Read, 9);
        r.push(Other);
        let h = suggest(Some("claude-opus-5-5"), &r).unwrap();
        assert_eq!(h.model, "haiku");
        assert!(h.reason.contains("9 of the last 10"));
    }

    #[test]
    fn any_edit_keeps_the_big_model() {
        let mut r = many(Read, 9);
        r.push(Edit);
        assert_eq!(suggest(Some("claude-opus-5-5"), &r), None);
    }

    #[test]
    fn planning_on_lighter_models_suggests_opus() {
        let r = [Plan, Read, Read, Plan, Read, Edit, Read, Read];
        assert_eq!(suggest(Some("claude-sonnet-5-5"), &r).unwrap().model, "opus");
        assert_eq!(suggest(Some("claude-opus-5-5"), &r), None);
    }

    #[test]
    fn coding_on_haiku_suggests_sonnet() {
        let r = [Edit, Run, Edit, Read, Run, Edit, Read, Run];
        assert_eq!(suggest(Some("claude-haiku-5-5"), &r).unwrap().model, "sonnet");
    }

    #[test]
    fn quiet_until_enough_calls_or_a_known_model() {
        assert_eq!(suggest(Some("claude-opus-5-5"), &many(Read, 5)), None);
        assert_eq!(suggest(None, &many(Read, 12)), None);
    }

    #[test]
    fn tool_kinds() {
        assert_eq!(tool_kind("Grep"), Read);
        assert_eq!(tool_kind("Write"), Edit);
        assert_eq!(tool_kind("Bash"), Run);
        assert_eq!(tool_kind("TodoWrite"), Plan);
        assert_eq!(tool_kind("mcp__x__y"), Other);
    }
}
