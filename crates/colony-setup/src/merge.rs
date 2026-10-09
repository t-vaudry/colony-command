//! Merging Colony's hook entries into a Claude Code `settings.json`, as text.
//!
//! The file is never re-serialized. Colony's entries are found by the tag in
//! their command, and only those are added, patched or removed; everything
//! else keeps its bytes, whitespace, key order, CRLFs and BOM.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use crate::json::{self, Kind, Node};

/// Every hook event Colony records (see the README).
pub const EVENTS: [&str; 14] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "PreCompact",
    "PostCompact",
];

/// Colony's entries end with this shell comment, then `v=<version> kind=<kind>`.
pub const TAG: &str = "# colony-setup";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// colony-hook: records the payload (and, on Windows, holds permission requests).
    Hook,
    /// colony-approve.sh: the WSL approval hook.
    Approve,
}

impl EntryKind {
    fn word(self) -> &'static str {
        match self {
            EntryKind::Hook => "hook",
            EntryKind::Approve => "approve",
        }
    }
}

/// An entry Colony wants present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub event: String,
    pub kind: EntryKind,
    /// The whole command, tag included.
    pub command: String,
    pub timeout: u64,
}

/// A Colony entry found in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Found {
    pub event: String,
    pub kind: EntryKind,
    pub command: String,
    pub timeout: Option<u64>,
    /// Version from the tag. `None` for an entry registered by hand.
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub event: String,
    pub kind: EntryKind,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Add,
    Update,
    Remove,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not valid JSON. The file is left alone.
    Invalid(String),
    /// Valid JSON in a shape this editor won't guess about.
    Shape(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(m) => write!(f, "settings.json isn't valid JSON ({m}); Colony won't touch it. Fix or remove the file and try again"),
            Error::Shape(m) => write!(f, "settings.json has an unexpected shape ({m}); Colony won't touch it"),
        }
    }
}
impl std::error::Error for Error {}

pub fn tag(kind: EntryKind, version: &str) -> String {
    format!("{TAG} v={version} kind={}", kind.word())
}

/// `command` followed by the tag.
pub fn tagged_command(command: &str, kind: EntryKind, version: &str) -> String {
    format!("{command} {}", tag(kind, version))
}

/// Whether `command` is one of Colony's, and what it is.
pub fn classify(command: &str) -> Option<(EntryKind, Option<String>)> {
    if let Some(at) = command.rfind(TAG) {
        let mut version = None;
        let mut kind = None;
        for tok in command[at + TAG.len()..].split_whitespace() {
            if let Some(v) = tok.strip_prefix("v=") {
                version = Some(v.to_string());
            } else if let Some(k) = tok.strip_prefix("kind=") {
                kind = match k {
                    "hook" => Some(EntryKind::Hook),
                    "approve" => Some(EntryKind::Approve),
                    _ => None,
                };
            }
        }
        if let Some(kind) = kind {
            return Some((kind, version));
        }
    }
    // Registered by hand, as the README used to say: the command runs one of our files.
    // Only the program being run counts, not an argument that happens to mention it.
    let mut toks = command.split_whitespace().map(|t| t.trim_matches(|c| c == '"' || c == '\''));
    let mut prog = toks.next()?;
    if matches!(prog, "sh" | "bash" | "exec") {
        prog = toks.next()?;
    }
    match prog.rsplit(['/', '\\']).next().unwrap_or(prog) {
        "colony-hook" | "colony-hook.exe" => Some((EntryKind::Hook, None)),
        "colony-approve.sh" => Some((EntryKind::Approve, None)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parsing and finding entries

const BOM: &str = "\u{feff}";

fn parse(text: &str) -> Result<Node, Error> {
    let start = if text.starts_with(BOM) { BOM.len() } else { 0 };
    let value: serde_json::Value = serde_json::from_str(&text[start..]).map_err(|e| Error::Invalid(e.to_string()))?;
    if !value.is_object() {
        return Err(Error::Shape("the top level isn't an object".into()));
    }
    json::scan(text, start).map_err(Error::Invalid)
}

/// A Colony hook object located in the tree.
struct Hit {
    group: usize,
    hook: usize,
    found: Found,
}

struct EventInfo {
    member: usize,
    name: String,
    hits: Vec<Hit>,
}

/// The `hooks` member (if any) and each event's Colony hits.
fn survey(text: &str, root: &Node) -> Result<(Option<usize>, Vec<EventInfo>), Error> {
    let members = root.members().expect("checked object");
    let mut found = members.iter().enumerate().filter(|(_, m)| m.key == "hooks");
    let Some((hi, hooks)) = found.next() else { return Ok((None, Vec::new())) };
    if found.next().is_some() {
        return Err(Error::Shape("\"hooks\" appears twice".into()));
    }
    let Some(events) = hooks.value.members() else {
        return Err(Error::Shape("\"hooks\" isn't an object".into()));
    };
    let mut out = Vec::new();
    for (mi, ev) in events.iter().enumerate() {
        // A malformed event we don't own is left alone; it only matters if we need to write to it.
        let Some(groups) = ev.value.items() else { continue };
        let mut hits = Vec::new();
        for (gi, g) in groups.iter().enumerate() {
            let Some(arr) = g.member("hooks").and_then(|m| m.value.items()) else { continue };
            for (hj, h) in arr.iter().enumerate() {
                let Some(cmd) = h.member("command").and_then(|m| m.value.as_str()) else { continue };
                if let Some((kind, version)) = classify(cmd) {
                    let timeout = h.member("timeout").and_then(|m| text[m.value.start..m.value.end].parse().ok());
                    hits.push(Hit {
                        group: gi,
                        hook: hj,
                        found: Found { event: ev.key.clone(), kind, command: cmd.to_string(), timeout, version },
                    });
                }
            }
        }
        out.push(EventInfo { member: mi, name: ev.key.clone(), hits });
    }
    Ok((Some(hi), out))
}

/// Colony's entries in the file, in file order.
pub fn inspect(text: &str) -> Result<Vec<Found>, Error> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root = parse(text)?;
    let (_, events) = survey(text, &root)?;
    Ok(events.into_iter().flat_map(|e| e.hits.into_iter().map(|h| h.found)).collect())
}

// ---------------------------------------------------------------------------
// Rendering new JSON in the file's own style

enum J {
    S(String),
    N(u64),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

struct Style {
    nl: &'static str,
    unit: String,
    multiline: bool,
}

fn detect_style(text: &str) -> Style {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut unit = "  ".to_string();
    for line in text.lines().skip(1) {
        let ws: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        if !ws.is_empty() && ws.len() < line.len() {
            unit = if ws.starts_with('\t') {
                "\t".into()
            } else if (1..=8).contains(&ws.len()) {
                ws
            } else {
                "  ".into()
            };
            break;
        }
    }
    Style { nl, unit, multiline: text.contains('\n') }
}

fn render(j: &J, indent: &str, pretty: bool, st: &Style, out: &mut String) {
    match j {
        J::S(s) => out.push_str(&serde_json::to_string(s).expect("string")),
        J::N(n) => out.push_str(&n.to_string()),
        J::A(v) if v.is_empty() => out.push_str("[]"),
        J::O(v) if v.is_empty() => out.push_str("{}"),
        J::A(v) => {
            let inner = format!("{indent}{}", st.unit);
            out.push('[');
            for (i, x) in v.iter().enumerate() {
                if pretty {
                    out.push_str(st.nl);
                    out.push_str(&inner);
                } else if i > 0 {
                    out.push(' ');
                }
                render(x, &inner, pretty, st, out);
                if i + 1 < v.len() {
                    out.push(',');
                }
            }
            if pretty {
                out.push_str(st.nl);
                out.push_str(indent);
            }
            out.push(']');
        }
        J::O(v) => {
            let inner = format!("{indent}{}", st.unit);
            out.push('{');
            for (i, (k, x)) in v.iter().enumerate() {
                if pretty {
                    out.push_str(st.nl);
                    out.push_str(&inner);
                } else if i > 0 {
                    out.push(' ');
                }
                out.push_str(&serde_json::to_string(k).expect("string"));
                out.push_str(": ");
                render(x, &inner, pretty, st, out);
                if i + 1 < v.len() {
                    out.push(',');
                }
            }
            if pretty {
                out.push_str(st.nl);
                out.push_str(indent);
            }
            out.push('}');
        }
    }
}

fn hook_json(w: &Wanted) -> J {
    J::O(vec![
        ("type".into(), J::S("command".into())),
        ("command".into(), J::S(w.command.clone())),
        ("timeout".into(), J::N(w.timeout)),
    ])
}

fn group_json(w: &Wanted) -> J {
    J::O(vec![("hooks".into(), J::A(vec![hook_json(w)]))])
}

// ---------------------------------------------------------------------------
// Rebuilding a container with elements deleted, replaced or appended

#[derive(Debug)]
enum Ed {
    Keep,
    Delete,
    Replace(String),
}

type Appends = Vec<(Option<String>, J)>;

fn line_indent(text: &str, pos: usize) -> &str {
    let line = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let rest = &text[line..];
    let n = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    &rest[..n]
}

fn all_keep(e: &[Ed]) -> bool {
    e.iter().all(|e| matches!(e, Ed::Keep))
}
fn all_delete(e: &[Ed]) -> bool {
    e.iter().all(|e| matches!(e, Ed::Delete))
}

/// The text of `node` (an object or array) with `edits` applied to its
/// elements (members for objects) and `appends` added at the end. Untouched
/// elements and the whitespace around them are copied byte for byte.
fn rebuild(text: &str, node: &Node, edits: &[Ed], appends: &Appends, st: &Style) -> String {
    let (open, close) = (node.start, node.end - 1);
    let (open_c, close_c) = if matches!(node.kind, Kind::Object(_)) { ('{', '}') } else { ('[', ']') };
    let spans: Vec<(usize, usize)> = match &node.kind {
        Kind::Object(m) => m.iter().map(|m| (m.start, m.value.end)).collect(),
        Kind::Array(a) => a.iter().map(|n| (n.start, n.end)).collect(),
        _ => unreachable!("rebuild is only called on containers"),
    };
    let n = spans.len();
    let container_indent = line_indent(text, open);
    let pretty = match n {
        0 => st.multiline,
        1 => text[open + 1..spans[0].0].contains('\n'),
        _ => text[spans[n - 2].1..spans[n - 1].0].contains('\n'),
    };
    let elem_indent: String = if n == 0 {
        format!("{container_indent}{}", st.unit)
    } else if pretty {
        line_indent(text, spans[n - 1].0).to_string()
    } else {
        String::new()
    };
    // What goes between two elements, and so before an appended one.
    let sep: String = match n {
        0 => {
            if pretty {
                format!(",{}{}", st.nl, elem_indent)
            } else {
                ", ".into()
            }
        }
        1 => format!(",{}", &text[open + 1..spans[0].0]),
        _ => text[spans[n - 2].1..spans[n - 1].0].to_string(),
    };

    let kept: Vec<usize> = (0..n).filter(|&i| !matches!(edits.get(i), Some(Ed::Delete))).collect();
    let mut out = String::new();
    out.push(open_c);
    if n > 0 && kept.is_empty() && appends.is_empty() {
        // Emptied: keep the closing bracket on its own line if it was.
        let trail = &text[spans[n - 1].1..close];
        out.push_str(if trail.contains('\n') { trail } else { "" });
        out.push(close_c);
        return out;
    }
    if n > 0 {
        out.push_str(&text[open + 1..spans[0].0]);
    } else if pretty {
        out.push_str(st.nl);
        out.push_str(&elem_indent);
    }
    for (pos, &i) in kept.iter().enumerate() {
        if pos > 0 {
            let prev = kept[pos - 1];
            out.push_str(&text[spans[prev].1..spans[prev + 1].0]);
        }
        match edits.get(i) {
            Some(Ed::Replace(t)) => out.push_str(t),
            _ => out.push_str(&text[spans[i].0..spans[i].1]),
        }
    }
    for (k, (key, value)) in appends.iter().enumerate() {
        if !kept.is_empty() || k > 0 {
            out.push_str(&sep);
        }
        if let Some(key) = key {
            out.push_str(&serde_json::to_string(key).expect("string"));
            out.push_str(": ");
        }
        render(value, &elem_indent, pretty, st, &mut out);
    }
    if n > 0 {
        out.push_str(&text[spans[n - 1].1..close]);
    } else if pretty {
        out.push_str(st.nl);
        out.push_str(container_indent);
    }
    out.push(close_c);
    out
}

// ---------------------------------------------------------------------------
// The merge

pub struct Merged {
    pub text: String,
    pub changes: Vec<Change>,
}

/// Makes the file's Colony entries exactly `wanted`: adds missing ones,
/// patches ones that differ (an older version, a changed path, a hand-made
/// registration), and removes the rest. `wanted` empty is uninstall.
/// `existing` is `None` when there is no file.
pub fn merge(existing: Option<&str>, wanted: &[Wanted]) -> Result<Merged, Error> {
    let blank = existing.is_none_or(|t| t.trim().is_empty());
    if blank {
        if wanted.is_empty() {
            return Ok(Merged { text: existing.unwrap_or("").to_string(), changes: Vec::new() });
        }
        let st = Style { nl: if existing.is_some_and(|t| t.contains("\r\n")) { "\r\n" } else { "\n" }, unit: "  ".into(), multiline: true };
        let order = order_events(wanted);
        let events = J::O(order.iter().map(|e| (e.clone(), J::A(wanted.iter().filter(|w| &w.event == e).map(group_json).collect()))).collect());
        let mut out = String::new();
        render(&J::O(vec![("hooks".into(), events)]), "", true, &st, &mut out);
        out.push_str(st.nl);
        let changes = wanted.iter().map(|w| Change { event: w.event.clone(), kind: w.kind, action: Action::Add }).collect();
        return Ok(Merged { text: out, changes });
    }
    let text = existing.expect("checked");
    let root = parse(text)?;
    let st = detect_style(text);
    let (hooks_idx, events) = survey(text, &root)?;
    let root_members = root.members().expect("object");

    let mut by_event: BTreeMap<&str, Vec<&Wanted>> = BTreeMap::new();
    for w in wanted {
        by_event.entry(w.event.as_str()).or_default().push(w);
    }
    if let Some(hi) = hooks_idx {
        for m in root_members[hi].value.members().expect("object") {
            if by_event.contains_key(m.key.as_str()) && m.value.items().is_none() {
                return Err(Error::Shape(format!("hooks.{} isn't a list", m.key)));
            }
        }
    }

    let mut changes = Vec::new();
    let mut member_edits: Vec<Ed> = Vec::new();
    if let Some(hi) = hooks_idx {
        let ev_members = root_members[hi].value.members().expect("object");
        member_edits = ev_members.iter().map(|_| Ed::Keep).collect();
        for ev in &events {
            let m = &ev_members[ev.member];
            let wants: Vec<&Wanted> = by_event.get(ev.name.as_str()).cloned().unwrap_or_default();
            if ev.hits.is_empty() && wants.is_empty() {
                continue;
            }
            let groups = m.value.items().expect("survey only lists arrays");
            let mut claimed = vec![false; wants.len()];
            let mut hook_edits: BTreeMap<usize, Vec<Ed>> = BTreeMap::new();
            for hit in &ev.hits {
                let g = &groups[hit.group];
                let harr = g.member("hooks").expect("hit").value.items().expect("hit");
                let edits = hook_edits.entry(hit.group).or_insert_with(|| harr.iter().map(|_| Ed::Keep).collect());
                match wants.iter().enumerate().position(|(i, w)| !claimed[i] && w.kind == hit.found.kind) {
                    Some(i) => {
                        claimed[i] = true;
                        let w = wants[i];
                        let h = &harr[hit.hook];
                        let same_to = h.member("timeout").is_some_and(|t| text[t.value.start..t.value.end].parse::<u64>().ok() == Some(w.timeout));
                        let is_command = h.member("type").and_then(|t| t.value.as_str()) == Some("command");
                        if !(hit.found.command == w.command && same_to && is_command) {
                            edits[hit.hook] = Ed::Replace(patch_hook(text, h, w, &st));
                            changes.push(Change { event: ev.name.clone(), kind: w.kind, action: Action::Update });
                        }
                    }
                    None => {
                        edits[hit.hook] = Ed::Delete;
                        changes.push(Change { event: ev.name.clone(), kind: hit.found.kind, action: Action::Remove });
                    }
                }
            }
            let mut group_edits: Vec<Ed> = groups.iter().map(|_| Ed::Keep).collect();
            for (gi, hedits) in &hook_edits {
                if all_keep(hedits) {
                    continue;
                }
                let g = &groups[*gi];
                if all_delete(hedits) {
                    group_edits[*gi] = Ed::Delete;
                } else {
                    let harr_m = g.member("hooks").expect("hit");
                    let new_arr = rebuild(text, &harr_m.value, hedits, &Vec::new(), &st);
                    group_edits[*gi] = Ed::Replace(format!("{}{}{}", &text[g.start..harr_m.value.start], new_arr, &text[harr_m.value.end..g.end]));
                }
            }
            let mut appends: Appends = Vec::new();
            for (i, w) in wants.iter().enumerate() {
                if !claimed[i] {
                    appends.push((None, group_json(w)));
                    changes.push(Change { event: ev.name.clone(), kind: w.kind, action: Action::Add });
                }
            }
            if all_keep(&group_edits) && appends.is_empty() {
                continue;
            }
            member_edits[ev.member] = if appends.is_empty() && all_delete(&group_edits) {
                Ed::Delete
            } else {
                let new_arr = rebuild(text, &m.value, &group_edits, &appends, &st);
                Ed::Replace(format!("{}{}", &text[m.start..m.value.start], new_arr))
            };
        }
    }

    // Events the file doesn't have yet, in the order Colony lists them.
    let existing_names: Vec<&str> = hooks_idx
        .map(|hi| root_members[hi].value.members().expect("object").iter().map(|m| m.key.as_str()).collect())
        .unwrap_or_default();
    let mut hook_appends: Appends = Vec::new();
    for e in order_events(wanted).into_iter().filter(|e| !existing_names.contains(&e.as_str())) {
        let ws = &by_event[e.as_str()];
        for w in ws {
            changes.push(Change { event: e.clone(), kind: w.kind, action: Action::Add });
        }
        hook_appends.push((Some(e.clone()), J::A(ws.iter().map(|w| group_json(w)).collect())));
    }

    if all_keep(&member_edits) && hook_appends.is_empty() {
        return Ok(Merged { text: text.to_string(), changes });
    }

    let mut root_edits: Vec<Ed> = root_members.iter().map(|_| Ed::Keep).collect();
    let mut root_appends: Appends = Vec::new();
    match hooks_idx {
        Some(hi) => {
            let hm = &root_members[hi];
            root_edits[hi] = if hook_appends.is_empty() && all_delete(&member_edits) && !member_edits.is_empty() {
                Ed::Delete
            } else {
                let new_obj = rebuild(text, &hm.value, &member_edits, &hook_appends, &st);
                Ed::Replace(format!("{}{}", &text[hm.start..hm.value.start], new_obj))
            };
        }
        None => {
            let obj = J::O(hook_appends.into_iter().map(|(k, v)| (k.expect("event"), v)).collect());
            root_appends.push((Some("hooks".into()), obj));
        }
    }
    let new_root = rebuild(text, &root, &root_edits, &root_appends, &st);
    Ok(Merged { text: format!("{}{}{}", &text[..root.start], new_root, &text[root.end..]), changes })
}

fn order_events(wanted: &[Wanted]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for w in wanted {
        if !seen.contains(&w.event) {
            seen.push(w.event.clone());
        }
    }
    seen
}

/// An existing hook object brought to `w`: command and timeout replaced in
/// place (so other keys and the layout stay), or the object rewritten if it
/// isn't a command hook.
fn patch_hook(text: &str, h: &Node, w: &Wanted, st: &Style) -> String {
    let members = h.members().expect("object");
    if h.member("type").and_then(|t| t.value.as_str()) != Some("command") {
        let mut out = String::new();
        render(&hook_json(w), line_indent(text, h.start), text[h.start..h.end].contains('\n'), st, &mut out);
        return out;
    }
    let cmd = serde_json::to_string(&w.command).expect("string");
    let edits: Vec<Ed> = members
        .iter()
        .map(|m| match m.key.as_str() {
            "command" => Ed::Replace(format!("{}{}", &text[m.start..m.value.start], cmd)),
            "timeout" => Ed::Replace(format!("{}{}", &text[m.start..m.value.start], w.timeout)),
            _ => Ed::Keep,
        })
        .collect();
    let mut appends: Appends = Vec::new();
    if h.member("timeout").is_none() {
        appends.push((Some("timeout".to_string()), J::N(w.timeout)));
    }
    rebuild(text, h, &edits, &appends, st)
}

/// Compares dotted versions numerically (`0.10.0` > `0.9.0`). A pre-release
/// suffix (`0.2.0-beta`) sorts before the release it leads up to.
pub fn classify_version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> (Vec<u64>, bool) {
        let (core, pre) = match s.split_once('-') {
            Some((c, _)) => (c, true),
            None => (s, false),
        };
        (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
    };
    let ((ac, ap), (bc, bp)) = (split(a), split(b));
    // No suffix is the later of two equal cores, hence the reversed flags.
    ac.cmp(&bc).then(bp.cmp(&ap))
}

#[cfg(test)]
mod version_tests {
    use super::classify_version_cmp as cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn versions() {
        assert_eq!(cmp("0.10.0", "0.9.0"), Greater);
        assert_eq!(cmp("0.2.0-beta", "0.2.0"), Less);
        assert_eq!(cmp("0.2.0", "0.2.0-beta"), Greater);
        assert_eq!(cmp("0.2.0", "0.2.0"), Equal);
        assert_eq!(cmp("0.2.1-rc1", "0.2.0"), Greater);
    }
}
