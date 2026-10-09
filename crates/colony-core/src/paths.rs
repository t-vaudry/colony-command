//! Project identity across Windows and WSL.
//!
//! `C:\code\api` seen from Windows and `/mnt/c/code/api` seen from WSL are the
//! same project, so both map to the key `c:/code/api`. Paths inside a distro's
//! own filesystem are keyed by distro.

use crate::event::HostId;

/// Where Colony puts the git worktrees it makes for isolated sessions,
/// relative to the repository root.
pub const WORKTREE_DIR: &str = ".colony/worktrees";

/// For a folder inside one of Colony's worktrees, the repository it belongs to
/// and the worktree's name: `/r/api/.colony/worktrees/fix-1a2b/src` ->
/// (`/r/api`, `fix-1a2b`). Bots in worktrees stay in their repository's district.
pub fn worktree_split(path: &str) -> Option<(String, String)> {
    let p = path.replace('\\', "/");
    let mark = format!("/{WORKTREE_DIR}/");
    let at = p.find(&mark)?;
    let name = p[at + mark.len()..].split('/').next().filter(|s| !s.is_empty())?;
    Some((p[..at].to_string(), name.to_string()))
}

/// The folder with any Colony worktree stripped back to its repository.
fn repo_dir(cwd: &str) -> &str {
    let mark = format!("/{WORKTREE_DIR}/");
    let norm_at = cwd.replace('\\', "/").find(&mark);
    match norm_at {
        // Byte offsets are unchanged: `\` and `/` are both one byte.
        Some(at) => &cwd[..at],
        None => cwd,
    }
}

/// Stable key for the project a session's cwd belongs to.
pub fn project_key(host: &HostId, cwd: &str) -> String {
    let cwd = repo_dir(cwd);
    if let Some(win) = windows_form(cwd) {
        return win;
    }
    match host {
        HostId::Windows => cwd.replace('\\', "/").trim_end_matches('/').to_lowercase(),
        HostId::Wsl(distro) => format!("wsl:{}:{}", distro.to_lowercase(), cwd.trim_end_matches('/')),
    }
}

/// Short display name: the last path component.
pub fn project_name(cwd: &str) -> String {
    let cwd = repo_dir(cwd);
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

/// A folder as seen from inside WSL: `C:\Users\x` -> `/mnt/c/Users/x`.
/// Linux paths are returned unchanged.
pub fn to_wsl_path(path: &str) -> String {
    let p = path.replace('\\', "/");
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        format!("/mnt/{}{}", (b[0] as char).to_ascii_lowercase(), &p[2..]).trim_end_matches('/').to_string()
    } else {
        p
    }
}

/// `C:\X\Y` or `/mnt/c/X/Y` -> `c:/x/y`. Windows paths are case-insensitive,
/// so the key is lowercased.
fn windows_form(path: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(p.trim_end_matches('/').to_lowercase());
    }
    let rest = p.strip_prefix("/mnt/")?;
    let mut parts = rest.splitn(2, '/');
    let drive = parts.next()?;
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let tail = parts.next().unwrap_or("");
    Some(format!("{}:/{}", drive, tail).trim_end_matches('/').to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_wsl_mount_paths_match() {
        let a = project_key(&HostId::Windows, r"C:\Users\thoma\code\Colony-Command");
        let b = project_key(&HostId::Wsl("Ubuntu".into()), "/mnt/c/Users/thoma/code/Colony-Command/");
        assert_eq!(a, "c:/users/thoma/code/colony-command");
        assert_eq!(a, b);
    }

    #[test]
    fn distro_paths_are_keyed_by_distro() {
        assert_eq!(project_key(&HostId::Wsl("Ubuntu".into()), "/home/thomas/api"), "wsl:ubuntu:/home/thomas/api");
    }

    #[test]
    fn wsl_paths() {
        assert_eq!(to_wsl_path(r"C:\Users\thoma\code\colony-command"), "/mnt/c/Users/thoma/code/colony-command");
        assert_eq!(to_wsl_path("D:/"), "/mnt/d");
        assert_eq!(to_wsl_path("/home/thomas"), "/home/thomas");
    }

    #[test]
    fn worktree_bots_share_their_repos_district() {
        let host = HostId::Windows;
        let repo = project_key(&host, r"C:\code\api");
        assert_eq!(project_key(&host, r"C:\code\api\.colony\worktrees\fix-1a2b\src"), repo);
        assert_eq!(project_name(r"C:\code\api\.colony\worktrees\fix-1a2b"), "api");
        let wsl = HostId::Wsl("Ubuntu".into());
        assert_eq!(project_key(&wsl, "/home/t/api/.colony/worktrees/x-1/"), project_key(&wsl, "/home/t/api"));
        assert_eq!(worktree_split("/home/t/api/.colony/worktrees/x-1/src"), Some(("/home/t/api".into(), "x-1".into())));
        assert_eq!(worktree_split("/home/t/api"), None);
    }

    #[test]
    fn names() {
        assert_eq!(project_name(r"C:\Users\thoma\code\colony-command"), "colony-command");
        assert_eq!(project_name("/home/thomas/api/"), "api");
    }
}
