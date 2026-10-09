//! Where agents work: the directory a tool call touches (rolled up so a
//! project doesn't produce dozens of buildings), and the keys the reducer uses
//! to notice two agents editing the same file or folder.
//!
//! Pure path handling, no I/O. Paths are compared the way `paths` keys
//! projects: `C:\x\y` and `/mnt/c/x/y` are the same place, and Windows paths
//! ignore case.

use serde::{Deserialize, Serialize};

use crate::event::HostId;

/// Directory levels below the project folder that make one building.
pub const BUILDING_DEPTH: usize = 2;
/// Two agents editing the same file within this long is a collision.
pub const FILE_WINDOW_MS: u64 = 120_000;
/// Two agents editing different files in one folder within this long is a
/// (weaker) collision.
pub const DIR_WINDOW_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionScope {
    File,
    Dir,
}

impl CollisionScope {
    pub fn window_ms(self) -> u64 {
        match self {
            CollisionScope::File => FILE_WINDOW_MS,
            CollisionScope::Dir => DIR_WINDOW_MS,
        }
    }
}

/// A warning that other agents are editing what this one is editing. Purely
/// informational: it never changes anyone's state and decays on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collision {
    pub scope: CollisionScope,
    /// The file, or the folder, as the tool call named it.
    pub path: String,
    /// The other agents (ids).
    pub with: Vec<String>,
    /// The latest edit that kept the warning alive.
    pub at: u64,
}

/// Files, lines added and removed, for work waiting on review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files: u32,
    pub added: u32,
    pub removed: u32,
}

/// A path split into components, drive first (`c:`) for Windows-style paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// Windows-style: compare without regard to case.
    pub ci: bool,
    pub comps: Vec<String>,
}

impl Parts {
    /// Absolute paths only; relative paths, URLs, globs and commands are None.
    pub fn parse(path: &str) -> Option<Parts> {
        let p = path.trim().replace('\\', "/");
        let b = p.as_bytes();
        let (ci, rest) = if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
            (true, format!("{}:/{}", (b[0] as char).to_ascii_lowercase(), &p[3..]))
        } else if let Some(r) = p.strip_prefix("/mnt/") {
            let mut it = r.splitn(2, '/');
            let drive = it.next().filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_alphabetic())?;
            (true, format!("{}:/{}", drive.to_ascii_lowercase(), it.next().unwrap_or("")))
        } else if p.starts_with('/') && !p.starts_with("//") {
            (false, p.clone())
        } else {
            return None;
        };
        if rest.contains(['*', '?', '<', '>', '|', '\n']) {
            return None;
        }
        let mut comps: Vec<String> = Vec::new();
        for c in rest.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    comps.pop();
                }
                c => comps.push(c.to_string()),
            }
        }
        (!comps.is_empty()).then_some(Parts { ci, comps })
    }

    fn same(&self, other: &str, mine: &str) -> bool {
        if self.ci {
            // The same fold `key` uses, so the two always agree.
            mine.to_lowercase() == other.to_lowercase()
        } else {
            mine == other
        }
    }

    /// Whether `self` is `root` or inside it.
    pub fn within(&self, root: &Parts) -> bool {
        self.ci == root.ci
            && self.comps.len() >= root.comps.len()
            && root.comps.iter().zip(&self.comps).all(|(r, c)| self.same(r, c))
    }

    /// Stable string for comparing places across hosts and spellings.
    pub fn key(&self, host: &HostId) -> String {
        let joined = self.comps.join("/");
        if self.ci {
            joined.to_lowercase()
        } else {
            match host {
                HostId::Windows => joined,
                HostId::Wsl(d) => format!("wsl:{}:{joined}", d.to_lowercase()),
            }
        }
    }
}

/// Whether the tool's target names a directory rather than a file.
fn names_dir(tool: &str, last: &str) -> bool {
    matches!(tool, "Grep" | "Glob" | "LS") && (!last.contains('.') || (last.starts_with('.') && !last[1..].contains('.')))
}

/// Tools whose target is a path worth placing on the map.
pub fn is_placing_tool(tool: &str) -> bool {
    matches!(tool, "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" | "NotebookRead" | "Grep" | "Glob" | "LS")
}

/// Tools that change a file. Only these can collide.
pub fn is_editing_tool(tool: &str) -> bool {
    matches!(tool, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

/// The folder a tool call works in, relative to the project folder and rolled
/// up to `BUILDING_DEPTH` levels (`src/auth/login.ts` -> `src/auth`; a file in
/// the project folder itself -> ``). None when the target isn't a path inside
/// the project.
pub fn building(tool: &str, root: &str, target: &str) -> Option<String> {
    if !is_placing_tool(tool) {
        return None;
    }
    let root = Parts::parse(root)?;
    let t = Parts::parse(target)?;
    if !t.within(&root) {
        return None;
    }
    let mut rel = &t.comps[root.comps.len()..];
    if let Some(last) = rel.last() {
        if !names_dir(tool, last) {
            rel = &rel[..rel.len() - 1];
        }
    }
    Some(rel.iter().take(BUILDING_DEPTH).cloned().collect::<Vec<_>>().join("/"))
}

/// Keys for an edit: (file, folder it's in, folder as written for display).
pub fn edit_keys(host: &HostId, target: &str) -> Option<(String, String, String)> {
    let p = Parts::parse(target)?;
    if p.comps.len() < 2 {
        return None;
    }
    let file = p.key(host);
    let dir = Parts { ci: p.ci, comps: p.comps[..p.comps.len() - 1].to_vec() }.key(host);
    let shown = target.trim().rsplit_once(['/', '\\']).map(|(d, _)| d.to_string()).unwrap_or_default();
    Some((file, dir, shown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_up_to_two_levels() {
        assert_eq!(building("Edit", "C:\\code\\api", "C:\\code\\api\\src\\auth\\jwt\\sign.ts").as_deref(), Some("src/auth"));
        assert_eq!(building("Read", "/home/me/api", "/home/me/api/README.md").as_deref(), Some(""));
        assert_eq!(building("Read", "/home/me/api", "/home/me/api/src/a.rs").as_deref(), Some("src"));
    }

    #[test]
    fn same_place_across_windows_and_wsl() {
        assert_eq!(building("Edit", "C:\\code\\api", "/mnt/c/code/api/src/a.ts").as_deref(), Some("src"));
        let w = edit_keys(&HostId::Windows, "C:\\Code\\API\\src\\a.ts").unwrap();
        let l = edit_keys(&HostId::Wsl("Ubuntu".into()), "/mnt/c/code/api/src/a.ts").unwrap();
        assert_eq!(w.0, l.0);
        assert_eq!(w.1, l.1);
    }

    #[test]
    fn ignores_non_paths_and_outside_targets() {
        assert_eq!(building("Grep", "/r/api", "TODO.*fix"), None);
        assert_eq!(building("Grep", "/r/api", "/api/v1/users"), None);
        assert_eq!(building("WebFetch", "/r/api", "https://example.com/x"), None);
        assert_eq!(building("Edit", "/r/api", "/r/other/src/a.rs"), None);
        assert_eq!(building("Bash", "/r/api", "/r/api/src/a.rs"), None);
        assert_eq!(building("Read", "/r/api", "src/a.rs"), None);
    }

    #[test]
    fn directory_tools_name_the_folder() {
        assert_eq!(building("Grep", "/r/api", "/r/api/src/auth").as_deref(), Some("src/auth"));
        assert_eq!(building("Grep", "/r/api", "/r/api/src/auth/jwt.ts").as_deref(), Some("src/auth"));
    }

    #[test]
    fn linux_paths_in_different_distros_differ() {
        let a = edit_keys(&HostId::Wsl("Ubuntu".into()), "/home/x/a.rs").unwrap();
        let b = edit_keys(&HostId::Wsl("Debian".into()), "/home/x/a.rs").unwrap();
        assert_ne!(a.0, b.0);
    }
}
