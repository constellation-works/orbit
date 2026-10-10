//! Machine-local task-publication repository binding contract.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::identity::{validate_machine_id, validate_registry_identifier};
use crate::workspace::WorkspaceError;

/// Default ordinary branch for a dedicated publication repository.
pub const DEFAULT_PUBLICATION_BRANCH: &str = "refs/heads/main";

/// Owner-local binding between one workspace and its dedicated publication
/// repository.
///
/// This record lives in machine-local workspace-registry state. It must not
/// appear in a task bundle or source-controlled `.orbit/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePublicationBinding {
    pub workspace_id: String,
    /// Registered portable source-repository identity, copied from the logical
    /// workspace `git_remote`. Never inferred from a checkout path.
    pub source_repository_fingerprint: String,
    pub publication_remote: String,
    pub publication_branch: String,
    /// Opaque lineage id. Unique among publication bindings on this machine.
    pub publication_id: String,
    pub authority_machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_commit: Option<String>,
}

impl WorkspacePublicationBinding {
    /// Validate field-level publication-binding rules.
    pub fn validated(self) -> Result<Self, WorkspaceError> {
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), WorkspaceError> {
        validate_registry_identifier("workspace_id", &self.workspace_id)
            .map_err(workspace_identity_error)?;
        validate_source_repository_fingerprint(&self.source_repository_fingerprint)?;
        validate_publication_remote(&self.publication_remote)?;
        validate_publication_branch(&self.publication_branch)?;
        validate_publication_id(&self.publication_id)?;
        validate_machine_id(&self.authority_machine_id).map_err(workspace_identity_error)?;
        validate_last_success(
            self.last_success_generation,
            self.last_success_commit.as_deref(),
        )?;
        if git_remotes_equivalent(
            &self.publication_remote,
            &self.source_repository_fingerprint,
        )? {
            return Err(WorkspaceError::Invalid(format!(
                "publication remote '{}' is equivalent to the workspace source remote",
                redact_git_remote(&self.publication_remote)
            )));
        }
        Ok(())
    }
}

/// Canonical `refs/heads/<name>` form, defaulting empty input to
/// [`DEFAULT_PUBLICATION_BRANCH`].
pub fn canonicalize_publication_branch(branch: &str) -> Result<String, WorkspaceError> {
    let trimmed = branch.trim();
    let candidate = if trimmed.is_empty() {
        DEFAULT_PUBLICATION_BRANCH.to_string()
    } else if let Some(name) = trimmed.strip_prefix("refs/heads/") {
        format!("refs/heads/{name}")
    } else if trimmed.starts_with("refs/") {
        return Err(WorkspaceError::Invalid(format!(
            "publication branch '{trimmed}' is not an ordinary refs/heads/* branch"
        )));
    } else {
        format!("refs/heads/{trimmed}")
    };
    validate_publication_branch(&candidate)?;
    Ok(candidate)
}

pub fn validate_publication_branch(branch: &str) -> Result<(), WorkspaceError> {
    let Some(name) = branch.strip_prefix("refs/heads/") else {
        return Err(WorkspaceError::Invalid(format!(
            "publication branch '{branch}' is not an ordinary refs/heads/* branch"
        )));
    };
    if name.is_empty() {
        return Err(WorkspaceError::Invalid(
            "publication branch must name a refs/heads/* ref".to_string(),
        ));
    }
    if !is_valid_git_ref_name(name) {
        return Err(WorkspaceError::Invalid(format!(
            "publication branch '{branch}' is not a valid ordinary Git branch ref"
        )));
    }
    Ok(())
}

pub fn validate_publication_id(publication_id: &str) -> Result<(), WorkspaceError> {
    validate_registry_identifier("publication_id", publication_id).map_err(workspace_identity_error)
}

pub fn validate_source_repository_fingerprint(fingerprint: &str) -> Result<(), WorkspaceError> {
    if fingerprint.trim() != fingerprint || fingerprint.is_empty() {
        return Err(WorkspaceError::Invalid(
            "source_repository_fingerprint must be the registered portable Git remote".to_string(),
        ));
    }
    if looks_like_local_path(fingerprint) {
        return Err(WorkspaceError::Invalid(
            "source_repository_fingerprint must be a portable Git remote, not a local checkout path"
                .to_string(),
        ));
    }
    if remote_has_credentials(fingerprint) {
        return Err(WorkspaceError::Invalid(format!(
            "source_repository_fingerprint '{}' must not contain credentials",
            redact_git_remote(fingerprint)
        )));
    }
    git_remote_identity(fingerprint).map(|_| ())
}

pub fn validate_publication_remote(remote: &str) -> Result<(), WorkspaceError> {
    if remote.trim() != remote || remote.is_empty() {
        return Err(WorkspaceError::Invalid(
            "publication remote must be a non-empty Git URL".to_string(),
        ));
    }
    if looks_like_local_path(remote) {
        return Err(WorkspaceError::Invalid(
            "publication remote must be a Git URL, not a local checkout path".to_string(),
        ));
    }
    if looks_like_remote_alias(remote) {
        return Err(WorkspaceError::Invalid(
            "publication remote must be a Git URL, not a checkout-local alias".to_string(),
        ));
    }
    if remote_has_credentials(remote) {
        return Err(WorkspaceError::Invalid(format!(
            "publication remote '{}' must not contain credentials",
            redact_git_remote(remote)
        )));
    }
    git_remote_identity(remote).map(|_| ())
}

pub fn validate_git_commit_id(commit: &str) -> Result<(), WorkspaceError> {
    let valid_len = commit.len() == 40 || commit.len() == 64;
    if valid_len && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(WorkspaceError::Invalid(
        "publication commit must be a 40- or 64-character Git object id".to_string(),
    ))
}

pub fn validate_last_success(
    generation: Option<u64>,
    commit: Option<&str>,
) -> Result<(), WorkspaceError> {
    match (generation, commit) {
        (None, None) => Ok(()),
        (Some(generation), Some(commit)) => {
            if generation == 0 {
                return Err(WorkspaceError::Invalid(
                    "publication last-success generation must be at least 1".to_string(),
                ));
            }
            validate_git_commit_id(commit)
        }
        _ => Err(WorkspaceError::Invalid(
            "publication last-success generation and commit must be recorded together".to_string(),
        )),
    }
}

/// Host/path identity used to compare Git remotes without credentials or `.git`.
pub fn git_remote_identity(remote: &str) -> Result<String, WorkspaceError> {
    let parsed = parse_git_remote(remote)?;
    Ok(parsed.identity)
}

/// The network host a Git remote names, lowercased and without a port.
/// `None` for a local path, a `file:` URL, or a value that is not a Git URL.
pub fn git_remote_network_host(remote: &str) -> Option<String> {
    if looks_like_local_path(remote) {
        return None;
    }
    let identity = parse_git_remote(remote).ok()?.identity;
    let authority = identity.split('/').next()?;
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => host,
        _ => authority,
    };
    (!host.is_empty()).then(|| host.to_string())
}

pub fn git_remotes_equivalent(left: &str, right: &str) -> Result<bool, WorkspaceError> {
    Ok(git_remote_identity(left)? == git_remote_identity(right)?)
}

/// Replace credential userinfo with `***`. Local paths become `<local-path>`.
/// Ambiguous scp-style prefixes containing `:` and `/` are treated as userinfo.
pub fn redact_git_remote(remote: &str) -> String {
    if looks_like_local_path(remote) {
        return "<local-path>".to_string();
    }
    if let Some(redacted) = redact_url_userinfo(remote).or_else(|| redact_raw_url_userinfo(remote))
    {
        return redacted;
    }
    if remote.contains("://") && Url::parse(remote).is_err() && remote.contains('@') {
        // A malformed URL can put password punctuation beyond the ordinary
        // authority boundary, so the location of userinfo is ambiguous.
        // Hide the whole remote rather than risk echoing any credential text.
        return "***".to_string();
    }
    redact_scp_userinfo(remote).unwrap_or_else(|| remote.to_string())
}

struct ParsedGitRemote {
    identity: String,
    has_credentials: bool,
}

fn parse_git_remote(remote: &str) -> Result<ParsedGitRemote, WorkspaceError> {
    if remote.contains("://") {
        return parse_url_remote(remote);
    }
    parse_scp_remote(remote)
}

fn parse_url_remote(remote: &str) -> Result<ParsedGitRemote, WorkspaceError> {
    let url = Url::parse(remote).map_err(|_| invalid_git_url(remote))?;
    match url.scheme() {
        "http" | "https" | "ssh" | "git" => {}
        "file" => {
            return Err(WorkspaceError::Invalid(
                "publication remote must be a Git URL, not a local checkout path".to_string(),
            ));
        }
        other => {
            return Err(WorkspaceError::Invalid(format!(
                "Git remote '{}' uses unsupported scheme '{other}'",
                redact_git_remote(remote)
            )));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid_git_url(remote))?
        .to_ascii_lowercase();
    let path = normalize_remote_path(url.path());
    if path.is_empty() {
        return Err(invalid_git_url(remote));
    }
    let identity = match url.port() {
        Some(port) if !is_default_port(url.scheme(), port) => {
            format!("{host}:{port}/{path}")
        }
        _ => format!("{host}/{path}"),
    };
    let has_credentials = url.password().is_some()
        || (!url.username().is_empty() && matches!(url.scheme(), "http" | "https"));
    Ok(ParsedGitRemote {
        identity,
        has_credentials,
    })
}

fn parse_scp_remote(remote: &str) -> Result<ParsedGitRemote, WorkspaceError> {
    // Userinfo can contain a password separator. Find the host/path separator
    // after it. A slash may be part of a password, so a colon-bearing prefix
    // fails closed as userinfo even if it could also be a repository path.
    let (username, host_path) = match remote.split_once('@') {
        Some((username, host_path))
            if (!username.contains('/') || username.contains(':')) && host_path.contains(':') =>
        {
            (username, host_path)
        }
        _ => ("", remote),
    };
    let Some((host, path)) = host_path.split_once(':') else {
        return Err(invalid_git_url(remote));
    };
    if host.contains('/') || path.is_empty() || path.starts_with('/') {
        return Err(invalid_git_url(remote));
    }
    if host.is_empty() || host.contains('@') {
        return Err(invalid_git_url(remote));
    }
    let has_credentials = username.contains(':');
    let identity = format!(
        "{}/{}",
        host.to_ascii_lowercase(),
        normalize_remote_path(path)
    );
    if identity.ends_with('/') {
        return Err(invalid_git_url(remote));
    }
    Ok(ParsedGitRemote {
        identity,
        has_credentials,
    })
}

fn normalize_remote_path(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    let without_git = trimmed
        .strip_suffix(".git")
        .or_else(|| trimmed.strip_suffix(".GIT"))
        .unwrap_or(trimmed);
    without_git.trim_matches('/').to_ascii_lowercase()
}

fn is_default_port(scheme: &str, port: u16) -> bool {
    matches!(
        (scheme, port),
        ("http", 80) | ("https", 443) | ("ssh", 22) | ("git", 9418)
    )
}

fn remote_has_credentials(remote: &str) -> bool {
    parse_git_remote(remote)
        .map(|parsed| parsed.has_credentials)
        .unwrap_or(false)
}

fn looks_like_local_path(remote: &str) -> bool {
    let trimmed = remote.trim();
    trimmed.starts_with("file:")
        || trimmed.starts_with('/')
        || trimmed.starts_with('.')
        || trimmed.starts_with('~')
        || (trimmed.len() >= 3
            && trimmed.as_bytes()[1] == b':'
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && (trimmed.as_bytes()[2] == b'\\' || trimmed.as_bytes()[2] == b'/'))
}

fn looks_like_remote_alias(remote: &str) -> bool {
    !remote.contains("://")
        && !remote.contains('@')
        && !remote.contains(':')
        && !remote.contains('/')
}

fn is_valid_git_ref_name(name: &str) -> bool {
    if name.is_empty()
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("//")
        || name.contains("..")
        || name.contains("@{")
        || name.ends_with(".lock")
    {
        return false;
    }
    name.split('/').all(is_valid_git_ref_component)
}

fn is_valid_git_ref_component(component: &str) -> bool {
    if component.is_empty() || component.starts_with('.') || component.ends_with(".lock") {
        return false;
    }
    component.chars().all(|ch| {
        !ch.is_ascii_control()
            && !matches!(
                ch,
                ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\t' | '\n'
            )
    })
}

fn redact_url_userinfo(remote: &str) -> Option<String> {
    let mut url = Url::parse(remote).ok()?;
    if url.password().is_none() && url.username().is_empty() {
        return None;
    }
    let _ = url.set_username("***");
    let _ = url.set_password(None);
    Some(url.to_string())
}

fn redact_raw_url_userinfo(remote: &str) -> Option<String> {
    let (scheme, remainder) = remote.split_once("://")?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let (_, host) = authority.rsplit_once('@')?;
    Some(format!(
        "{scheme}://***@{host}{}",
        &remainder[authority_end..]
    ))
}

fn redact_scp_userinfo(remote: &str) -> Option<String> {
    let (username, host_path) = remote.split_once('@')?;
    let (host, path) = host_path.split_once(':')?;
    if !username.contains(':') && username != "***" {
        return None;
    }
    // Passwords may contain at-signs even when parsing rejects the remote.
    // Mask through the last one in the host while keeping the original suffix
    // intact. Rebuilding around the first colon would alter bracketed IPv6
    // addresses such as `[::1]`.
    if let Some((_, host)) = host.rsplit_once('@') {
        return Some(format!("***@{host}:{path}"));
    }
    // A colon after the first at-sign makes a later at-sign ambiguous: it may
    // still be part of the password. Hide the whole remote rather than risk
    // exposing password text or guessing where the repository path begins.
    let first_colon = host_path.find(':')?;
    if let Some(later_at) = host_path[first_colon + 1..].find('@') {
        let later_at = first_colon + 1 + later_at;
        if host_path
            .find('/')
            .is_none_or(|path_start| later_at < path_start)
        {
            return Some("***".to_string());
        }
    }
    Some(format!("***@{host_path}"))
}

fn invalid_git_url(remote: &str) -> WorkspaceError {
    WorkspaceError::Invalid(format!(
        "Git remote '{}' is not a valid Git URL",
        redact_git_remote(remote)
    ))
}

fn workspace_identity_error(error: crate::identity::IdentityError) -> WorkspaceError {
    WorkspaceError::Invalid(error.to_string())
}
