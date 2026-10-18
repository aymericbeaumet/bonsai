use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use serde::{Deserialize, Serialize};

use crate::picker;
use crate::repo::repo_id_from_url;

/// GitHub account type. Absent means the owner is not classified yet, or the
/// host is not GitHub. Either search prefix matches an unclassified owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    User,
    #[serde(alias = "organization")]
    Organisation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerId {
    pub host: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub free: Vec<String>,
    pub user: Vec<String>,
    pub organisation: Vec<String>,
}

pub struct OwnerResolved {
    pub key: String,
    pub kind: OwnerKind,
}

pub struct OwnerJob {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<OwnerResolved>,
}

impl OwnerJob {
    pub fn try_recv(&self) -> Option<OwnerResolved> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for OwnerJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Host of a remote, including `host/repo` URLs that have no owner.
pub fn host_from_remote(remote: &str) -> Option<String> {
    let id = repo_id_from_url(remote)?;
    let host = id.split('/').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// First path segment after the host, when the remote has an owner and a repo.
pub fn owner_from_remote(remote: &str) -> Option<OwnerId> {
    let id = repo_id_from_url(remote)?;
    let mut parts = id.split('/');
    let host = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    parts.next()?;
    if host.is_empty() || name.is_empty() {
        return None;
    }
    Some(OwnerId { host, name })
}

pub fn cache_key(owner: &OwnerId) -> String {
    format!(
        "{}/{}",
        owner.host.to_lowercase(),
        owner.name.to_lowercase()
    )
}

pub fn cache_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::home_dir()?.join(".config"),
    };
    Some(base.join("bonsai").join("owners.json"))
}

pub fn load(path: &Path) -> BTreeMap<String, OwnerKind> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(path: &Path, kinds: &BTreeMap<String, OwnerKind>) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(kinds).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, text)
}

/// `user:` and `organisation:` (`org:`, `organization:`) are owner filters.
/// A prefix with no pattern is ignored so the list stays put while it is typed.
pub fn parse_query(raw: &str) -> Query {
    let mut query = Query::default();
    for token in raw.split_whitespace() {
        let lower = token.to_lowercase();
        if let Some(pattern) = qualifier(&lower, "user:") {
            if !pattern.is_empty() {
                query.user.push(pattern.to_string());
            }
            continue;
        }
        if let Some(pattern) = organisation_pattern(&lower) {
            if !pattern.is_empty() {
                query.organisation.push(pattern.to_string());
            }
            continue;
        }
        query.free.push(lower);
    }
    query
}

pub fn matches(
    query: &Query,
    owner: Option<&OwnerId>,
    kind: Option<OwnerKind>,
    haystack: &str,
) -> bool {
    qualifier_matches(owner, kind, &query.user, Some(OwnerKind::User))
        && qualifier_matches(
            owner,
            kind,
            &query.organisation,
            Some(OwnerKind::Organisation),
        )
        && query
            .free
            .iter()
            .all(|token| picker::fuzzy_match(haystack, token))
}

pub fn parse_github_type(text: &str) -> Option<OwnerKind> {
    match text.trim() {
        "User" => Some(OwnerKind::User),
        "Organization" => Some(OwnerKind::Organisation),
        _ => None,
    }
}

/// Classify GitHub logins that are not cached yet. Results arrive on the
/// receiver; dropping the job abandons lookups that have not started.
pub fn start(names: Vec<String>) -> OwnerJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    if !names.is_empty() {
        let cancel_worker = Arc::clone(&cancel);
        std::thread::spawn(move || {
            for name in names {
                if cancel_worker.load(Ordering::Relaxed) {
                    break;
                }
                let Some(kind) = classify_github_owner(&name) else {
                    continue;
                };
                if sender
                    .send(OwnerResolved {
                        key: format!("github.com/{name}"),
                        kind,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    OwnerJob { cancel, receiver }
}

fn classify_github_owner(name: &str) -> Option<OwnerKind> {
    let login = github_login(name)?;
    let endpoint = format!("users/{login}");
    let output = Command::new("gh")
        .args(["api", &endpoint, "--jq", ".type"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    parse_github_type(&text)
}

fn github_login(name: &str) -> Option<&str> {
    if name.is_empty()
        || name.starts_with('-')
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return None;
    }
    Some(name)
}

fn qualifier<'a>(token: &'a str, prefix: &str) -> Option<&'a str> {
    token.strip_prefix(prefix)
}

fn organisation_pattern(token: &str) -> Option<&str> {
    for prefix in ["organisation:", "organization:", "org:"] {
        if let Some(pattern) = token.strip_prefix(prefix) {
            return Some(pattern);
        }
    }
    None
}

fn qualifier_matches(
    owner: Option<&OwnerId>,
    kind: Option<OwnerKind>,
    patterns: &[String],
    expected: Option<OwnerKind>,
) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let Some(owner) = owner else {
        return false;
    };
    let name = owner.name.to_lowercase();
    if !patterns.iter().all(|pattern| name.contains(pattern)) {
        return false;
    }
    match (kind, expected) {
        (None, _) => true,
        (Some(actual), Some(expected)) => actual == expected,
        (Some(_), None) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_is_the_segment_between_host_and_repo() {
        assert_eq!(
            owner_from_remote("git@github.com:interfaceinc/tma-clients.git"),
            Some(OwnerId {
                host: "github.com".to_string(),
                name: "interfaceinc".to_string(),
            })
        );
        assert_eq!(
            owner_from_remote("https://github.com/aymericbeaumet/bonsai.git"),
            Some(OwnerId {
                host: "github.com".to_string(),
                name: "aymericbeaumet".to_string(),
            })
        );
        assert_eq!(
            owner_from_remote("git@git.moria.zone:system/foundations.git")
                .as_ref()
                .map(|owner| owner.name.as_str()),
            Some("system")
        );
        assert_eq!(owner_from_remote("git@example:bonsai.git"), None);
        assert_eq!(owner_from_remote(""), None);
        assert_eq!(
            host_from_remote("https://github.com/aymericbeaumet/bonsai.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            host_from_remote("git@example:bonsai.git").as_deref(),
            Some("example")
        );
        assert_eq!(host_from_remote(""), None);
    }

    #[test]
    fn qualifiers_are_case_insensitive_and_an_empty_pattern_is_ignored() {
        let query = parse_query("  User:aym  ORG:inter  feature ");
        assert_eq!(query.user, vec!["aym".to_string()]);
        assert_eq!(query.organisation, vec!["inter".to_string()]);
        assert_eq!(query.free, vec!["feature".to_string()]);
        assert_eq!(parse_query("organisation:"), Query::default());
        assert_eq!(
            parse_query("organization:Inter"),
            Query {
                organisation: vec!["inter".to_string()],
                ..Query::default()
            }
        );
    }

    #[test]
    fn known_kind_excludes_the_other_prefix_and_unknown_matches_both() {
        let user = OwnerId {
            host: "github.com".to_string(),
            name: "aymericbeaumet".to_string(),
        };
        let org = OwnerId {
            host: "github.com".to_string(),
            name: "interfaceinc".to_string(),
        };
        let user_query = parse_query("user:aymeric");
        let org_query = parse_query("organisation:interface");
        assert!(matches(&user_query, Some(&user), Some(OwnerKind::User), ""));
        assert!(!matches(&org_query, Some(&user), Some(OwnerKind::User), ""));
        assert!(matches(
            &org_query,
            Some(&org),
            Some(OwnerKind::Organisation),
            ""
        ));
        assert!(!matches(
            &user_query,
            Some(&org),
            Some(OwnerKind::Organisation),
            ""
        ));
        assert!(matches(&user_query, Some(&user), None, ""));
        assert!(matches(
            &parse_query("organisation:aymeric"),
            Some(&user),
            None,
            ""
        ));
        assert!(!matches(&org_query, Some(&user), None, ""));
        assert!(!matches(&org_query, None, None, "interfaceinc"));
        assert!(matches(
            &parse_query("organisation:interface dev"),
            Some(&org),
            None,
            "interfaceinc other ab/dev"
        ));
        assert!(!matches(
            &parse_query("organisation:interface dev"),
            Some(&org),
            None,
            "interfaceinc other feature"
        ));
    }

    #[test]
    fn github_type_and_cache_round_trip() {
        assert_eq!(parse_github_type("User\n"), Some(OwnerKind::User));
        assert_eq!(
            parse_github_type(" Organization "),
            Some(OwnerKind::Organisation)
        );
        assert_eq!(parse_github_type("Bot"), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owners.json");
        let mut kinds = BTreeMap::new();
        kinds.insert(
            "github.com/interfaceinc".to_string(),
            OwnerKind::Organisation,
        );
        save(&path, &kinds).unwrap();
        assert_eq!(load(&path), kinds);
        assert!(load(&dir.path().join("missing.json")).is_empty());
    }
}
