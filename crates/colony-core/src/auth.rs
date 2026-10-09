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

/// Which sign-in an agent is waiting on. Serialized for the map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthNeed {
    pub provider: String,
    pub label: String,
}

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// The sign-in a failed tool's error is asking for, if it is.
pub fn detect(error: &str) -> Option<AuthNeed> {
    let e = error.to_lowercase();
    PROVIDERS
        .iter()
        .find(|p| p.patterns.iter().any(|pat| e.contains(pat)))
        .map(|p| AuthNeed { provider: p.id.into(), label: p.label.into() })
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
