//! Where Colony's files go: the Windows user profile, or a WSL distro reached
//! through `wsl.exe`. Paths are strings with forward slashes; for a distro
//! they are Linux paths.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What is being written, which decides how carefully.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A program: may replace a running copy by moving it aside.
    Executable,
    /// A plain file Colony owns.
    Data,
    /// A file the user owns (settings.json, its backup): a symlink is followed rather than
    /// replaced, permissions are kept, and nothing is ever moved aside.
    Config,
}

pub trait TargetFs {
    /// The user's home folder as a path on this target.
    fn home(&self) -> io::Result<String>;
    /// `None` when the file doesn't exist.
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>>;
    /// Writes atomically (temp file, then rename), creating folders.
    /// `mode_from` names an existing file whose permissions the new one copies (Config only).
    fn write(&self, path: &str, data: &[u8], kind: Kind, mode_from: Option<&str>) -> io::Result<()>;
    /// Removes a file; a missing one is fine.
    fn remove(&self, path: &str) -> io::Result<()>;
    /// `x86_64` or `aarch64`.
    fn arch(&self) -> io::Result<String>;
}

/// The machine Colony is running on (or a folder standing in for a home).
pub struct LocalFs {
    pub home: PathBuf,
}

impl TargetFs for LocalFs {
    fn home(&self) -> io::Result<String> {
        Ok(self.home.to_string_lossy().replace('\\', "/"))
    }

    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, path: &str, data: &[u8], kind: Kind, mode_from: Option<&str>) -> io::Result<()> {
        // A settings file that is a symlink (dotfile managers, a repo) is written through.
        let mut dest = PathBuf::from(path);
        if kind == Kind::Config && std::fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
            dest = std::fs::canonicalize(&dest)?;
        }
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = PathBuf::from(format!("{}.colony-tmp", dest.display()));
        write_private(&tmp, data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = match kind {
                Kind::Executable => 0o755,
                Kind::Data => 0o644,
                Kind::Config => mode_from
                    .and_then(|p| std::fs::metadata(p).ok())
                    .or_else(|| std::fs::metadata(&dest).ok())
                    .map_or(0o644, |m| m.permissions().mode() & 0o7777),
            };
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode_from;
        match std::fs::rename(&tmp, &dest) {
            Ok(()) => Ok(()),
            Err(e) if kind != Kind::Executable => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
            Err(_) => {
                // A running program can't be overwritten on Windows, but it can be
                // renamed: move it aside, then put the new file in its place.
                let aside = PathBuf::from(format!("{}.old", dest.display()));
                let _ = std::fs::remove_file(&aside);
                if dest.exists() {
                    std::fs::rename(&dest, &aside).inspect_err(|_| {
                        let _ = std::fs::remove_file(&tmp);
                    })?;
                }
                std::fs::rename(&tmp, &dest)?;
                let _ = std::fs::remove_file(&aside);
                Ok(())
            }
        }
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                // Running right now: rename it out of the way so the name is free.
                let aside = format!("{path}.old");
                let _ = std::fs::remove_file(&aside);
                std::fs::rename(path, &aside).map_err(|_| e)
            }
        }
    }

    fn arch(&self) -> io::Result<String> {
        Ok(match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            _ => "x86_64",
        }
        .into())
    }
}

/// Creates `path` readable by the owner only, so a private file is never briefly wider.
fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(data)
}

/// A WSL distro, driven through `wsl.exe --exec sh`. Starts the distro if it
/// is stopped.
pub struct WslFs {
    pub distro: String,
    /// Stand-in for `$HOME`, for tests.
    pub home_override: Option<String>,
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl WslFs {
    fn sh(&self, script: &str, args: &[&str], stdin: Option<&[u8]>) -> io::Result<std::process::Output> {
        let mut cmd = Command::new("wsl.exe");
        cmd.args(["-d", &self.distro, "--exec", "sh", "-c", script, "sh"]).args(args);
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(not(windows))]
        let _ = CREATE_NO_WINDOW;
        let mut child = cmd.spawn()?;
        if let Some(data) = stdin {
            let mut si = child.stdin.take().expect("piped");
            // Written from a thread so a large file can't deadlock against a full stdout pipe.
            let data = data.to_vec();
            let t = std::thread::spawn(move || {
                let _ = si.write_all(&data);
            });
            let out = child.wait_with_output()?;
            let _ = t.join();
            return Ok(out);
        }
        child.wait_with_output()
    }

    fn check(&self, out: std::process::Output, what: &str) -> io::Result<Vec<u8>> {
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(io::Error::other(format!("{what} in {}: {}", self.distro, err.trim())))
        }
    }
}

impl TargetFs for WslFs {
    fn home(&self) -> io::Result<String> {
        if let Some(h) = &self.home_override {
            return Ok(h.clone());
        }
        let out = self.sh("printf %s \"$HOME\"", &[], None)?;
        let home = String::from_utf8_lossy(&self.check(out, "reading $HOME")?).into_owned();
        if home.starts_with('/') {
            Ok(home)
        } else {
            Err(io::Error::other(format!("{} gave no home folder", self.distro)))
        }
    }

    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        let out = self.sh("[ -e \"$1\" ] || exit 44; cat -- \"$1\"", &[path], None)?;
        if out.status.code() == Some(44) {
            return Ok(None);
        }
        self.check(out, &format!("reading {path}")).map(Some)
    }

    fn write(&self, path: &str, data: &[u8], kind: Kind, mode_from: Option<&str>) -> io::Result<()> {
        let default_mode = if kind == Kind::Executable { "755" } else { "644" };
        // Follow a symlink (readlink -f), keep the mode of the file being replaced (or of
        // `mode_from`), and keep the temp file private until it has that mode.
        let mode = "m=$(stat -L -c %a -- \"$3\" 2>/dev/null || stat -L -c %a -- \"$t\" 2>/dev/null || echo \"$2\")";
        let script = format!(
            "umask 077; mkdir -p -- \"$(dirname -- \"$1\")\" && t=$(readlink -f -- \"$1\") && {} && \
             cat > \"$t.colony-tmp\" && chmod \"$m\" \"$t.colony-tmp\" && mv -f -- \"$t.colony-tmp\" \"$t\"",
            if kind == Kind::Config { mode } else { "m=\"$2\"" }
        );
        let like = if kind == Kind::Config { mode_from.unwrap_or("") } else { "" };
        let out = self.sh(&script, &[path, default_mode, like], Some(data))?;
        self.check(out, &format!("writing {path}")).map(|_| ())
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        let out = self.sh("rm -f -- \"$1\"", &[path], None)?;
        self.check(out, &format!("removing {path}")).map(|_| ())
    }

    fn arch(&self) -> io::Result<String> {
        let out = self.sh("uname -m", &[], None)?;
        let m = String::from_utf8_lossy(&self.check(out, "uname")?).trim().to_string();
        Ok(if m == "aarch64" || m == "arm64" { "aarch64".into() } else { "x86_64".into() })
    }
}

/// wsl.exe writes UTF-16LE when its output is redirected.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes.iter().skip(1).step_by(2).all(|&b| b == 0) {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
