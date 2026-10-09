//! Setting Colony up on a machine: the Claude Code hooks, the WSL probe and
//! the approval hook, on Windows and in each WSL distro.
//!
//! Everything goes through `status` (what is installed), `plan` (what would
//! change, with a diff of `settings.json`) and `apply` (back up, then write).
//! Uninstall is `apply` with nothing selected. The settings file is edited as
//! text by [`merge`], so the user's own settings are never reformatted.
//!
//! Fail safe: entries run a shell snippet that does nothing and exits 0 unless
//! the Colony binary is present, and Colony's binaries never exit 2 (the only
//! code that would block Claude Code). Installing copies binaries first and
//! writes the settings last; uninstalling edits the settings first.

pub mod fs;
pub mod json;
pub mod merge;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use fs::{Kind, LocalFs, TargetFs, WslFs};
use merge::{classify_version_cmp, EntryKind, Found, Wanted, EVENTS};

/// The version this build installs. Entries and the bin folder carry it so
/// an older install can be spotted.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Held permission requests wait this long for an answer on the map.
const APPROVAL_TIMEOUT: u64 = 600;
const HOOK_TIMEOUT: u64 = 5;

// ---------------------------------------------------------------------------
// Options, targets

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Stand-in for the Windows user profile (tests).
    pub user_home: Option<PathBuf>,
    /// Colony's data folder on Windows; defaults to `COLONY_HOME`, then `<home>/.colony`.
    pub colony_home: Option<PathBuf>,
    /// Stand-in for `$HOME` inside every distro (tests).
    pub wsl_home: Option<String>,
    /// Where to look for the files to install, before the usual places.
    pub bundle_dirs: Vec<PathBuf>,
}

impl Options {
    pub fn from_env() -> Options {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        Options {
            user_home: var("COLONY_SETUP_USER_HOME").map(PathBuf::from),
            colony_home: var("COLONY_HOME").map(PathBuf::from),
            wsl_home: var("COLONY_SETUP_WSL_HOME").map(|v| v.to_string_lossy().into_owned()),
            bundle_dirs: var("COLONY_BUNDLE_DIR").map(PathBuf::from).into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Windows,
    Wsl,
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetInfo {
    /// `windows` or `wsl:<distro>`.
    pub id: String,
    pub label: String,
    pub kind: TargetKind,
    /// A stopped distro is started by looking inside it, so it isn't read until asked.
    pub running: bool,
}

/// Windows (when this is Windows, or a test home is given) and every WSL distro.
pub fn targets(opts: &Options) -> Vec<TargetInfo> {
    let mut out = Vec::new();
    if cfg!(windows) || opts.user_home.is_some() {
        out.push(TargetInfo { id: "windows".into(), label: "Windows".into(), kind: TargetKind::Windows, running: true });
    }
    if cfg!(windows) {
        out.extend(list_distros());
    }
    out
}

fn list_distros() -> Vec<TargetInfo> {
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["--list", "--verbose"]).stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(out) = cmd.output() else { return Vec::new() };
    parse_distro_list(&fs::decode_wsl_output(&out.stdout))
}

pub fn parse_distro_list(text: &str) -> Vec<TargetInfo> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}').trim_start_matches('*').trim())
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let (name, state) = (cols.next()?, cols.next()?);
            if name == "NAME" || name.starts_with("docker-desktop") || cols.next().is_none() {
                return None;
            }
            Some(TargetInfo { id: format!("wsl:{name}"), label: format!("WSL · {name}"), kind: TargetKind::Wsl, running: state.eq_ignore_ascii_case("running") })
        })
        .collect()
}

fn fs_for(opts: &Options, id: &str) -> Result<(Box<dyn TargetFs>, TargetKind), String> {
    if id == "windows" {
        let home = opts
            .user_home
            .clone()
            .or_else(|| std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from))
            .ok_or("can't find the user profile")?;
        Ok((Box::new(LocalFs { home }), TargetKind::Windows))
    } else if let Some(d) = id.strip_prefix("wsl:") {
        Ok((Box::new(WslFs { distro: d.to_string(), home_override: opts.wsl_home.clone() }), TargetKind::Wsl))
    } else {
        Err(format!("unknown target {id}"))
    }
}

// ---------------------------------------------------------------------------
// What gets installed

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Selection {
    pub hooks: bool,
    pub probe: bool,
    pub approval: bool,
}

impl Selection {
    pub const ALL: Selection = Selection { hooks: true, probe: true, approval: true };
    pub const NONE: Selection = Selection { hooks: false, probe: false, approval: false };
}

struct Layout {
    kind: TargetKind,
    bin_dir: String,
    /// The bin folder as a hook command names it.
    bin_ref: String,
    settings: String,
    arch: String,
}

fn layout(opts: &Options, fs: &dyn TargetFs, kind: TargetKind) -> Result<Layout, String> {
    let home = fs.home().map_err(|e| e.to_string())?.trim_end_matches('/').to_string();
    let default_colony = format!("{home}/.colony");
    let colony = match (kind, &opts.colony_home) {
        (TargetKind::Windows, Some(c)) => c.to_string_lossy().replace('\\', "/"),
        _ => default_colony.clone(),
    };
    let bin_dir = format!("{colony}/bin");
    let bin_ref = if colony.eq_ignore_ascii_case(&default_colony) { "$HOME/.colony/bin".to_string() } else { bin_dir.clone() };
    Ok(Layout {
        kind,
        settings: format!("{home}/.claude/settings.json"),
        bin_dir,
        bin_ref,
        arch: fs.arch().map_err(|e| e.to_string())?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileId {
    Hook,
    Setup,
    Probe,
    Approve,
}

const ALL_FILES: [FileId; 4] = [FileId::Hook, FileId::Setup, FileId::Probe, FileId::Approve];

impl FileId {
    fn dest_name(self, kind: TargetKind) -> &'static str {
        match (self, kind) {
            (FileId::Hook, TargetKind::Windows) => "colony-hook.exe",
            (FileId::Hook, _) => "colony-hook",
            (FileId::Setup, _) => "colony-setup.exe",
            (FileId::Probe, _) => "colony-probe",
            (FileId::Approve, _) => "colony-approve.sh",
        }
    }
    fn executable(self) -> bool {
        true
    }
    /// Not having it in the bundle is not an error.
    fn optional(self) -> bool {
        matches!(self, FileId::Setup)
    }
    fn applies(self, kind: TargetKind) -> bool {
        match kind {
            TargetKind::Windows => matches!(self, FileId::Hook | FileId::Setup),
            TargetKind::Wsl => !matches!(self, FileId::Setup),
        }
    }
}

fn files_for(kind: TargetKind, sel: Selection) -> Vec<FileId> {
    ALL_FILES
        .into_iter()
        .filter(|f| f.applies(kind))
        .filter(|f| match (kind, f) {
            (TargetKind::Windows, _) => sel.hooks || sel.approval,
            (TargetKind::Wsl, FileId::Hook) => sel.hooks,
            (TargetKind::Wsl, FileId::Probe) => sel.probe,
            (TargetKind::Wsl, FileId::Approve) => sel.approval,
            _ => false,
        })
        .collect()
}

fn hook_command(l: &Layout) -> String {
    let p = format!("{}/{}", l.bin_ref, FileId::Hook.dest_name(l.kind));
    format!("[ -x \"{p}\" ] && exec \"{p}\"; exit 0")
}

fn approve_command(l: &Layout) -> String {
    let p = format!("{}/colony-approve.sh", l.bin_ref);
    format!("[ -f \"{p}\" ] && exec sh \"{p}\"; exit 0")
}

fn wanted(l: &Layout, sel: Selection) -> Vec<Wanted> {
    let mut out = Vec::new();
    let mk = |event: &str, kind: EntryKind, cmd: String, timeout: u64| Wanted {
        event: event.to_string(),
        kind,
        command: merge::tagged_command(&cmd, kind, VERSION),
        timeout,
    };
    match l.kind {
        TargetKind::Windows => {
            for e in EVENTS {
                let perm = e == "PermissionRequest";
                if sel.hooks || (sel.approval && perm) {
                    out.push(mk(e, EntryKind::Hook, hook_command(l), if perm && sel.approval { APPROVAL_TIMEOUT } else { HOOK_TIMEOUT }));
                }
            }
        }
        TargetKind::Wsl => {
            if sel.hooks {
                for e in EVENTS {
                    out.push(mk(e, EntryKind::Hook, hook_command(l), HOOK_TIMEOUT));
                }
            }
            if sel.approval {
                out.push(mk("PermissionRequest", EntryKind::Approve, approve_command(l), APPROVAL_TIMEOUT));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The files to install

/// Finds the files Colony installs: next to the app, in its resources, or in a
/// development tree.
pub struct Bundle {
    dirs: Vec<PathBuf>,
}

impl Bundle {
    pub fn discover(extra: &[PathBuf]) -> Bundle {
        let mut dirs: Vec<PathBuf> = extra.to_vec();
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            dirs.push(exe_dir.clone());
            dirs.push(exe_dir.join("resources"));
            dirs.push(exe_dir.join("..").join("resources"));
            // A development tree: target/<profile>/ inside the repository. Only then; an
            // installed copy must not pick up files from folders above it.
            let in_build_tree = exe_dir.components().any(|c| c.as_os_str() == "target");
            for anc in exe_dir.ancestors().skip(1).take(4).filter(|_| in_build_tree) {
                dirs.push(anc.join("hooks"));
                for t in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
                    dirs.push(anc.join("target").join(t).join("release"));
                }
                dirs.push(anc.join("app").join("src-tauri").join("resources"));
            }
        }
        Bundle { dirs }
    }

    fn find(&self, file: FileId, kind: TargetKind, arch: &str) -> Option<PathBuf> {
        let name = file.dest_name(kind);
        let triple = format!("{arch}-unknown-linux-musl");
        self.dirs.iter().find_map(|d| {
            let linux_only = matches!(kind, TargetKind::Wsl) && matches!(file, FileId::Hook | FileId::Probe);
            let mut candidates = vec![d.join("linux").join(arch).join(name)];
            // A plain `colony-hook` beside a Windows exe isn't a Linux build; one in a musl target folder is.
            if !linux_only || d.to_string_lossy().replace('\\', "/").contains(&triple) {
                candidates.push(d.join(name));
            }
            candidates.into_iter().find(|p| p.is_file())
        })
    }
}

fn sha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Status

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Nothing of it is installed.
    Missing,
    /// Some of it is installed.
    Partial,
    /// Installed by an older Colony (or registered by hand).
    Outdated,
    /// Same version, but differs from what this build would install.
    Changed,
    Current,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComponentStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub what: &'static str,
    pub state: State,
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetStatus {
    pub target: TargetInfo,
    /// The target wasn't read (a stopped distro) or couldn't be.
    pub checked: bool,
    pub error: Option<String>,
    pub components: Vec<ComponentStatus>,
    pub warnings: Vec<String>,
    pub settings_path: Option<String>,
}

fn components_for(kind: TargetKind) -> Vec<(&'static str, &'static str, &'static str)> {
    match kind {
        TargetKind::Windows => vec![
            ("hooks", "Hooks", "Registers colony-hook for every Claude Code event, so sessions show up on the map"),
            ("approval", "Approvals", "Lets permission requests be answered on the map (Allow / Deny), and steps aside when the map is closed"),
        ],
        TargetKind::Wsl => vec![
            ("hooks", "Hooks", "Registers colony-hook for every Claude Code event in this distro"),
            ("probe", "Probe", "Installs colony-probe, which streams this distro's sessions to Colony"),
            ("approval", "Approval hook", "Registers colony-approve.sh so permission requests can be answered on the map"),
        ],
    }
}

fn selection_with(id: &str) -> Selection {
    Selection { hooks: id == "hooks", probe: id == "probe", approval: id == "approval" }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Missing,
    Current,
    Outdated,
    Changed,
}

fn aggregate(items: &[Item]) -> State {
    if items.iter().all(|i| *i == Item::Current) {
        State::Current
    } else if items.iter().all(|i| *i == Item::Missing) {
        State::Missing
    } else if items.contains(&Item::Missing) {
        State::Partial
    } else if items.contains(&Item::Outdated) {
        State::Outdated
    } else {
        State::Changed
    }
}

/// How an installed entry compares with the wanted one.
fn entry_item(found: &[Found], w: &Wanted, ignore_timeout: bool) -> Item {
    let Some(f) = found.iter().find(|f| f.event == w.event && f.kind == w.kind) else { return Item::Missing };
    if f.command == w.command && (ignore_timeout || f.timeout == Some(w.timeout)) {
        return Item::Current;
    }
    match &f.version {
        None => Item::Outdated,
        Some(v) if classify_version_cmp(v, VERSION).is_lt() => Item::Outdated,
        Some(_) => Item::Changed,
    }
}

struct Read {
    layout: Layout,
    settings_text: Option<String>,
}

fn read_target(opts: &Options, id: &str) -> Result<(Box<dyn TargetFs>, Read), String> {
    let (fs, kind) = fs_for(opts, id)?;
    let l = layout(opts, fs.as_ref(), kind)?;
    let settings_text = match fs.read(&l.settings).map_err(|e| format!("can't read {}: {e}", l.settings))? {
        None => None,
        Some(b) => Some(String::from_utf8(b).map_err(|_| format!("{} isn't UTF-8 text; Colony won't touch it", l.settings))?),
    };
    Ok((fs, Read { layout: l, settings_text }))
}

pub fn status(opts: &Options, target: &TargetInfo) -> TargetStatus {
    let mut st = TargetStatus { target: target.clone(), checked: false, error: None, components: Vec::new(), warnings: Vec::new(), settings_path: None };
    if !target.running {
        return st;
    }
    match status_inner(opts, target, &mut st) {
        Ok(()) => st.checked = true,
        Err(e) => st.error = Some(e),
    }
    st
}

fn status_inner(opts: &Options, target: &TargetInfo, st: &mut TargetStatus) -> Result<(), String> {
    let (fs, read) = read_target(opts, &target.id)?;
    let l = &read.layout;
    st.settings_path = Some(l.settings.clone());
    let found = match &read.settings_text {
        Some(t) => merge::inspect(t).map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    if read.settings_text.as_deref().is_some_and(|t| t.contains(".colony/capture")) {
        st.warnings.push("An older Colony capture hook is also registered in settings.json. It records the same events; remove it by hand (see spikes/capture/README.md).".into());
    }
    let bundle = Bundle::discover(&opts.bundle_dirs);
    let installed_version = fs.read(&format!("{}/VERSION", l.bin_dir)).ok().flatten().map(|b| String::from_utf8_lossy(&b).trim().to_string());
    for (id, label, what) in components_for(l.kind) {
        let sel = selection_with(id);
        let mut items = Vec::new();
        // Windows: hooks own every event, approvals own PermissionRequest's long timeout.
        let full = Selection { hooks: true, probe: true, approval: true };
        let mut all = wanted(l, full);
        if l.kind == TargetKind::Windows && id == "hooks" {
            for w in all.iter().filter(|w| w.event != "PermissionRequest") {
                items.push(entry_item(&found, w, false));
            }
            if let Some(w) = all.iter().find(|w| w.event == "PermissionRequest") {
                items.push(entry_item(&found, w, true));
            }
        } else if l.kind == TargetKind::Windows {
            all.retain(|w| w.event == "PermissionRequest");
            for w in &all {
                items.push(entry_item(&found, w, false));
            }
        } else {
            for w in all.iter().filter(|w| (id == "hooks") == (w.kind == EntryKind::Hook) && id != "probe") {
                items.push(entry_item(&found, w, false));
            }
        }
        let file_ids: Vec<FileId> = match (l.kind, id) {
            (TargetKind::Windows, "hooks") => files_for(l.kind, Selection { hooks: true, ..Selection::NONE }),
            (TargetKind::Windows, _) => Vec::new(),
            _ => files_for(l.kind, sel),
        };
        // colony-setup.exe is a convenience copy; whether it is there says nothing about the install.
        for f in file_ids.into_iter().filter(|f| *f != FileId::Setup) {
            let have = fs.read(&format!("{}/{}", l.bin_dir, f.dest_name(l.kind))).map_err(|e| e.to_string())?;
            let want = bundle.find(f, l.kind, &l.arch).and_then(|p| std::fs::read(p).ok());
            items.push(match (have, want) {
                (None, _) => Item::Missing,
                (Some(h), Some(w)) if sha(&h) != sha(&w) => match &installed_version {
                    Some(v) if classify_version_cmp(v, VERSION).is_ge() => Item::Changed,
                    _ => Item::Outdated,
                },
                (Some(_), _) => match &installed_version {
                    Some(v) if classify_version_cmp(v, VERSION).is_lt() => Item::Outdated,
                    _ => Item::Current,
                },
            });
        }
        st.components.push(ComponentStatus { id, label, what, state: aggregate(&items) });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan and apply

#[derive(Clone, Debug, Serialize)]
pub struct FileOp {
    pub path: String,
    /// `create`, `update`, `remove` or `same`.
    pub action: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub target: String,
    pub settings_path: String,
    /// Unified diff of settings.json; empty when it won't change.
    pub diff: String,
    pub changes: Vec<merge::Change>,
    pub files: Vec<FileOp>,
    /// Where the backup of settings.json will go, when it exists and will change.
    pub backup_path: Option<String>,
    /// Nothing to do.
    pub nothing: bool,
    /// Why this can't go ahead; nothing is written when set.
    pub error: Option<String>,
    /// Identifies this exact plan. `apply` refuses if what it would do has changed since.
    pub token: String,
}

struct Prepared {
    plan: Plan,
    new_settings: Option<String>,
    /// (dest, bytes, kind)
    writes: Vec<(String, Vec<u8>, Kind)>,
    removes: Vec<String>,
    /// settings.json as read when planning; the write is refused if it has changed since.
    base: Option<String>,
}

fn prepare(opts: &Options, id: &str, sel: Selection) -> Result<(Box<dyn TargetFs>, Prepared), String> {
    let (fs, read) = read_target(opts, id)?;
    let l = &read.layout;
    let mut plan = Plan {
        target: id.to_string(),
        settings_path: l.settings.clone(),
        diff: String::new(),
        changes: Vec::new(),
        files: Vec::new(),
        backup_path: None,
        nothing: false,
        error: None,
        token: String::new(),
    };
    let merged = merge::merge(read.settings_text.as_deref(), &wanted(l, sel)).map_err(|e| e.to_string())?;
    plan.changes = merged.changes;
    let mut new_settings = None;
    if read.settings_text.as_deref() != Some(merged.text.as_str()) && !(read.settings_text.is_none() && merged.text.is_empty()) {
        plan.diff = diff(read.settings_text.as_deref().unwrap_or(""), &merged.text);
        if read.settings_text.is_some() {
            plan.backup_path = Some(format!("{}.colony-backup-{}", l.settings, timestamp()));
        }
        new_settings = Some(merged.text);
    }

    let bundle = Bundle::discover(&opts.bundle_dirs);
    let needed = files_for(l.kind, sel);
    let mut writes = Vec::new();
    for f in &needed {
        let dest = format!("{}/{}", l.bin_dir, f.dest_name(l.kind));
        let Some(src) = bundle.find(*f, l.kind, &l.arch) else {
            if f.optional() {
                continue;
            }
            return Err(format!(
                "{} isn't in this build of Colony{}",
                f.dest_name(l.kind),
                if l.kind == TargetKind::Wsl { format!(" (for {})", l.arch) } else { String::new() }
            ));
        };
        let bytes = std::fs::read(&src).map_err(|e| format!("can't read {}: {e}", src.display()))?;
        let have = fs.read(&dest).map_err(|e| e.to_string())?;
        let action = match &have {
            None => "create",
            Some(h) if sha(h) == sha(&bytes) => "same",
            Some(_) => "update",
        };
        plan.files.push(FileOp { path: dest.clone(), action });
        if action != "same" {
            writes.push((dest, bytes, if f.executable() { Kind::Executable } else { Kind::Data }));
        }
    }
    let mut removes = Vec::new();
    // colony-setup.exe is never removed by itself (it may be the one running); the uninstaller deletes it.
    for f in ALL_FILES.into_iter().filter(|f| f.applies(l.kind) && !needed.contains(f) && *f != FileId::Setup) {
        let dest = format!("{}/{}", l.bin_dir, f.dest_name(l.kind));
        if fs.read(&dest).map_err(|e| e.to_string())?.is_some() {
            plan.files.push(FileOp { path: dest.clone(), action: "remove" });
            removes.push(dest);
        }
    }
    let version_file = format!("{}/VERSION", l.bin_dir);
    if !needed.is_empty() && fs.read(&version_file).ok().flatten().as_deref() != Some(format!("{VERSION}\n").as_bytes()) {
        writes.push((version_file, format!("{VERSION}\n").into_bytes(), Kind::Data));
    }
    plan.nothing = plan.diff.is_empty() && writes.is_empty() && removes.is_empty();
    let mut h = Sha256::new();
    h.update(new_settings.as_deref().unwrap_or("").as_bytes());
    for (p, b, _) in &writes {
        h.update(p.as_bytes());
        h.update(b);
    }
    for r in &removes {
        h.update(r.as_bytes());
    }
    plan.token = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let base = read.settings_text.clone();
    Ok((fs, Prepared { plan, new_settings, writes, removes, base }))
}

/// What installing `sel` on `target` would do. An error in the plan itself
/// (settings.json isn't valid JSON, a file is missing from the build) is in
/// `Plan::error`.
pub fn plan(opts: &Options, target: &str, sel: Selection) -> Plan {
    match prepare(opts, target, sel) {
        Ok((_, p)) => p.plan,
        Err(e) => Plan {
            target: target.to_string(),
            settings_path: String::new(),
            diff: String::new(),
            changes: Vec::new(),
            files: Vec::new(),
            backup_path: None,
            nothing: false,
            error: Some(e),
            token: String::new(),
        },
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub what: String,
    pub ok: bool,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Applied {
    pub target: String,
    pub ok: bool,
    pub steps: Vec<Step>,
}

/// Makes `target` match `sel`: unselected components are removed. With
/// `expect`, refuses if the plan is no longer the one the user confirmed.
pub fn apply(opts: &Options, target: &str, sel: Selection, expect: Option<&str>) -> Applied {
    let mut steps = Vec::new();
    let mut step = |what: String, r: Result<(), String>| {
        let ok = r.is_ok();
        steps.push(Step { what, ok, detail: r.err() });
        ok
    };
    let (fs, p) = match prepare(opts, target, sel) {
        Ok(x) => x,
        Err(e) => {
            step("Check the plan".into(), Err(e));
            return Applied { target: target.into(), ok: false, steps };
        }
    };
    if let Some(t) = expect {
        if t != p.plan.token {
            step("Check the plan".into(), Err("things changed since the preview (settings.json or the installed files); review the preview again".into()));
            return Applied { target: target.into(), ok: false, steps };
        }
    }
    if p.plan.nothing {
        step("Already up to date".into(), Ok(()));
        return Applied { target: target.into(), ok: true, steps };
    }
    let uninstalling = p.new_settings.is_some() && p.writes.is_empty();
    let write_files = |steps: &mut Vec<Step>| -> bool {
        let mut ok = true;
        for (dest, bytes, kind) in &p.writes {
            let r = fs.write(dest, bytes, *kind, None).map_err(|e| e.to_string());
            let good = r.is_ok();
            steps.push(Step { what: format!("Write {dest}"), ok: good, detail: r.err() });
            ok &= good;
        }
        ok
    };
    let write_settings = |steps: &mut Vec<Step>| -> bool {
        let Some(new) = &p.new_settings else { return true };
        let path = p.plan.settings_path.as_str();
        // Compare-and-swap: Claude Code (or the user) may have saved the file since it was
        // read for the plan; writing over that would lose their change. (A save in the few
        // milliseconds between this check and the rename can't be ruled out.)
        let now = match fs.read(path) {
            Ok(b) => b.map(|b| String::from_utf8_lossy(&b).into_owned()),
            Err(e) => {
                steps.push(Step { what: "Re-read settings.json".into(), ok: false, detail: Some(e.to_string()) });
                return false;
            }
        };
        if now != p.base {
            steps.push(Step {
                what: "Check settings.json".into(),
                ok: false,
                detail: Some("settings.json changed while Colony was working; nothing was written to it. Review the preview again".into()),
            });
            return false;
        }
        // The backup is the bytes just checked, with the original's permissions.
        if let Some(backup) = &p.plan.backup_path {
            let r = fs.write(backup, p.base.as_deref().unwrap_or("").as_bytes(), Kind::Config, Some(path)).map_err(|e| e.to_string());
            let good = r.is_ok();
            steps.push(Step { what: format!("Back up settings.json to {backup}"), ok: good, detail: r.err() });
            if !good {
                return false;
            }
        }
        let r = fs.write(path, new.as_bytes(), Kind::Config, Some(path)).map_err(|e| e.to_string());
        let good = r.is_ok();
        steps.push(Step { what: format!("Update {}", p.plan.settings_path), ok: good, detail: r.err() });
        good
    };
    // Install: files first, so an entry never points at nothing. Uninstall: entries first.
    let ok = if uninstalling {
        write_settings(&mut steps)
    } else {
        write_files(&mut steps) && write_settings(&mut steps)
    };
    let mut ok = ok;
    if ok || uninstalling {
        for r in &p.removes {
            let res = fs.remove(r).map_err(|e| e.to_string());
            let good = res.is_ok();
            steps.push(Step { what: format!("Remove {r}"), ok: good, detail: res.err() });
            ok &= good;
        }
    }
    if uninstalling && ok {
        // Nothing of ours is left in bin/.
        if sel == Selection::NONE {
            if let Ok(l) = fs_for(opts, target).and_then(|(f, k)| layout(opts, f.as_ref(), k)) {
                let _ = fs.remove(&format!("{}/VERSION", l.bin_dir));
            }
        }
    }
    Applied { target: target.into(), ok, steps }
}

fn diff(before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(2)
        .header("settings.json (now)", "settings.json (after)")
        .to_string()
}

/// `YYYYMMDD-HHMMSS` in UTC.
pub fn timestamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    format_timestamp(secs)
}

pub fn format_timestamp(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(format_timestamp(0), "19700101-000000");
        assert_eq!(format_timestamp(1_760_000_000), "20251009-085320");
    }

    #[test]
    fn distro_list() {
        let t = "  NAME              STATE           VERSION\r\n* Ubuntu            Running         2\r\n  docker-desktop    Stopped         2\r\n  Debian            Stopped         2\r\n";
        let d = parse_distro_list(t);
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].id.as_str(), d[0].running), ("wsl:Ubuntu", true));
        assert_eq!((d[1].id.as_str(), d[1].running), ("wsl:Debian", false));
    }
}
