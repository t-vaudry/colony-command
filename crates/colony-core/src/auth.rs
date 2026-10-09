//! Spotting a bot that is stuck on a login, and how to fix it.
//!
//! Tools like `gh`, `az` or `aws` fail the same way when you aren't signed
//! in, and a bot can't sign in for you: it needs a person at a browser. Each
//! provider here says how its failures read, which commands sign in, and which
//! command proves the login worked afterwards.

use serde::{Deserialize, Serialize};

pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    /// Lowercase fragments of a tool error that mean "not signed in".
    patterns: &'static [&'static str],
    /// Commands that sign in, run one after another in a terminal.
    pub login: &'static [&'static [&'static str]],
    /// Succeeds only when signed in.
    pub check: &'static [&'static str],
}

pub const PROVIDERS: &[Provider] = &[
    // The bot's own login, not a tool it runs: it shows up as a failed turn.
    Provider {
        id: "claude",
        label: "Claude",
        patterns: &["authentication_failed", "invalid api key", "please run /login", "oauth token has expired", "run claude auth login"],
        login: &[&["claude", "auth", "login"]],
        check: &["claude", "auth", "status"],
    },
    Provider {
        id: "github",
        label: "GitHub",
        patterns: &[
            "gh auth login",
            "not logged into any github hosts",
            "gh_token environment variable",
            "could not read username for 'https://github.com",
            "authentication failed for 'https://github.com",
            "bad credentials",
            "http 401: requires authentication",
        ],
        // setup-git lets plain `git push`/`fetch` over https use the same login.
        login: &[&["gh", "auth", "login"], &["gh", "auth", "setup-git"]],
        check: &["gh", "auth", "status"],
    },
    Provider {
        id: "gitlab",
        label: "GitLab",
        patterns: &["glab auth login", "could not read username for 'https://gitlab.com"],
        login: &[&["glab", "auth", "login"]],
        check: &["glab", "auth", "status"],
    },
    Provider {
        id: "azure",
        label: "Azure",
        patterns: &["az login", "please run 'az login'"],
        login: &[&["az", "login"]],
        check: &["az", "account", "show"],
    },
    Provider {
        id: "aws",
        label: "AWS",
        patterns: &["aws sso login", "sso session associated with this profile has expired", "unable to locate credentials"],
        login: &[&["aws", "sso", "login"]],
        check: &["aws", "sts", "get-caller-identity"],
    },
    Provider {
        id: "gcloud",
        label: "Google Cloud",
        patterns: &["gcloud auth login", "gcloud auth application-default login"],
        login: &[&["gcloud", "auth", "login"]],
        check: &["gcloud", "auth", "print-access-token"],
    },
    Provider {
        id: "docker",
        label: "Docker registry",
        patterns: &["docker login"],
        login: &[&["docker", "login"]],
        check: &["docker", "info"],
    },
    Provider {
        id: "npm",
        label: "npm",
        patterns: &["npm login", "npm adduser"],
        login: &[&["npm", "login"]],
        check: &["npm", "whoami"],
    },
];

/// A program that is easy to install for a bot that finds it missing.
pub struct Tool {
    pub id: &'static str,
    pub label: &'static str,
    /// Command names a "not found" error can name.
    names: &'static [&'static str],
    /// winget package id, for Windows.
    pub winget: &'static str,
    /// Shell command that installs it where `apt-get` exists (WSL).
    pub apt: &'static str,
    /// Succeeds only when it is installed.
    pub check: &'static [&'static str],
}

pub const TOOLS: &[Tool] = &[
    Tool { id: "gh", label: "GitHub CLI", names: &["gh"], winget: "GitHub.cli", apt: "sudo apt-get install -y gh", check: &["gh", "--version"] },
    Tool { id: "glab", label: "GitLab CLI", names: &["glab"], winget: "GLab.GLab", apt: "sudo apt-get install -y glab", check: &["glab", "--version"] },
    Tool { id: "git", label: "Git", names: &["git"], winget: "Git.Git", apt: "sudo apt-get install -y git", check: &["git", "--version"] },
    Tool { id: "node", label: "Node.js", names: &["node", "npm", "npx"], winget: "OpenJS.NodeJS.LTS", apt: "sudo apt-get install -y nodejs npm", check: &["node", "--version"] },
    Tool { id: "az", label: "Azure CLI", names: &["az"], winget: "Microsoft.AzureCLI", apt: "curl -sL https://aka.ms/InstallAzureCLIDeb | sudo bash", check: &["az", "--version"] },
    Tool { id: "aws", label: "AWS CLI", names: &["aws"], winget: "Amazon.AWSCLI", apt: "sudo apt-get install -y awscli", check: &["aws", "--version"] },
    Tool { id: "jq", label: "jq", names: &["jq"], winget: "jqlang.jq", apt: "sudo apt-get install -y jq", check: &["jq", "--version"] },
    Tool { id: "rg", label: "ripgrep", names: &["rg"], winget: "BurntSushi.ripgrep.MSVC", apt: "sudo apt-get install -y ripgrep", check: &["rg", "--version"] },
];

pub fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}

/// What a stuck bot needs from the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedKind {
    /// Not signed in: `provider` names an entry of `PROVIDERS`.
    #[default]
    SignIn,
    /// Not installed: `provider` names an entry of `TOOLS`.
    Install,
}

/// Which sign-in or install an agent is waiting on. Serialized for the map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthNeed {
    #[serde(default)]
    pub kind: NeedKind,
    /// Id of the provider or tool.
    pub provider: String,
    pub label: String,
}

impl AuthNeed {
    /// Why the agent is blocked, in a few words.
    pub fn reason(&self) -> String {
        match self.kind {
            NeedKind::SignIn => format!("{} needs you to sign in", self.label),
            NeedKind::Install => format!("{} isn't installed", self.label),
        }
    }
}

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// What a failed tool's error is asking the user for, if anything: a login,
/// or a program that isn't installed.
pub fn detect(error: &str) -> Option<AuthNeed> {
    let e = error.to_lowercase();
    PROVIDERS
        .iter()
        .find(|p| p.patterns.iter().any(|pat| e.contains(pat)))
        .map(|p| AuthNeed { kind: NeedKind::SignIn, provider: p.id.into(), label: p.label.into() })
        .or_else(|| {
            let t = missing_tool(&e)?;
            Some(AuthNeed { kind: NeedKind::Install, provider: t.id.into(), label: t.label.into() })
        })
}

/// The known program a shell says it couldn't find: bash ("x: command not
/// found"), PowerShell ("The term 'x' is not recognized") or cmd ("'x' is not
/// recognized as an internal or external command"). `e` is lowercase.
fn missing_tool(e: &str) -> Option<&'static Tool> {
    let mut named: Vec<&str> = Vec::new();
    for (at, _) in e.match_indices(": command not found") {
        named.extend(e[..at].rsplit(|c: char| c.is_whitespace() || c == ':').next());
    }
    for start in e.match_indices("the term '").map(|(i, m)| i + m.len()) {
        named.extend(e[start..].split('\'').next());
    }
    for (at, _) in e.match_indices("' is not recognized as an internal or external command") {
        named.extend(e[..at].rsplit('\'').next());
    }
    named.into_iter().find_map(|n| {
        let n = n.trim().trim_end_matches(".exe");
        let n = n.rsplit(['/', '\\']).next().unwrap_or(n);
        TOOLS.iter().find(|t| t.names.contains(&n))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(s: &str) -> Option<String> {
        detect(s).map(|n| n.provider)
    }

    #[test]
    fn recognizes_signin_errors() {
        assert_eq!(found("Exit code 4\nTo get started with GitHub CLI, please run:  gh auth login"), Some("github".into()));
        assert_eq!(found("fatal: could not read Username for 'https://github.com': terminal prompts disabled"), Some("github".into()));
        assert_eq!(found("ERROR: Please run 'az login' to setup account."), Some("azure".into()));
        assert_eq!(found("The SSO session associated with this profile has expired"), Some("aws".into()));
    }

    fn missing(s: &str) -> Option<String> {
        detect(s).filter(|n| n.kind == NeedKind::Install).map(|n| n.provider)
    }

    #[test]
    fn recognizes_missing_programs_from_each_shell() {
        assert_eq!(missing("Exit code 127\n/bin/bash: line 1: gh: command not found"), Some("gh".into()));
        assert_eq!(missing("bash: rg: command not found"), Some("rg".into()));
        assert_eq!(missing("gh : The term 'gh' is not recognized as the name of a cmdlet, function, script file"), Some("gh".into()));
        assert_eq!(missing("'jq' is not recognized as an internal or external command,\noperable program or batch file."), Some("jq".into()));
        assert_eq!(missing("bash: C:\\Program Files\\nodejs\\npm: command not found"), Some("node".into()));
    }

    #[test]
    fn unknown_missing_programs_are_not_offered() {
        assert_eq!(detect("bash: frobnicate: command not found"), None);
        // A sign-in problem wins over a missing tool when both show up.
        assert_eq!(found("gh auth login required; also foo: command not found"), Some("github".into()));
    }

    #[test]
    fn every_tool_has_something_to_run() {
        for t in TOOLS {
            assert!(!t.winget.is_empty() && !t.apt.is_empty() && !t.check.is_empty(), "{}", t.id);
        }
    }

    #[test]
    fn recognizes_claudes_own_login_failing() {
        assert_eq!(found("authentication_failed"), Some("claude".into()));
        assert_eq!(found("Invalid API key · Please run /login"), Some("claude".into()));
        assert_eq!(found("OAuth token has expired. Please run claude auth login"), Some("claude".into()));
    }

    #[test]
    fn ordinary_failures_are_not_auth() {
        assert_eq!(found("error: pathspec 'x' did not match any file(s) known to git"), None);
        assert_eq!(found("npm ERR! missing script: build"), None);
    }

    #[test]
    fn every_provider_has_something_to_run() {
        for p in PROVIDERS {
            assert!(!p.login.is_empty() && !p.check.is_empty(), "{}", p.id);
        }
    }
}
