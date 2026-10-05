//! Grants: the operator's answer to a manifest's `permissions` request.
//!
//! A manifest *requests* (`spec.permissions`, `spec.backend.sandbox`); the
//! operator *grants* at `orbit plugin enable --grant …`. Only the second is
//! authority (design §4.1). This module is the one mapping from request to
//! grant name, so the loader, `orbit plugin show`, and the audit row all
//! agree on which grants a plugin needs.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use super::manifest::{PluginManifest, PluginNetworkPermission, PluginSandbox};

/// One grantable capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginGrant {
    /// `permissions.fs.read` / `permissions.fs.write`: the paths the sandbox
    /// opens for the backend.
    Fs,
    /// `permissions.network: loopback | any`.
    Network,
    /// `permissions.env_pass`: parent variables copied into the child.
    EnvPass,
    /// `permissions.orbit_tools`: callbacks through `orbit tool run`.
    OrbitTools,
    /// `backend.sandbox: none`: the backend runs unconfined.
    Unsandboxed,
}

impl PluginGrant {
    /// Every grant, in the order surfaces list them.
    pub const ALL: [PluginGrant; 5] = [
        PluginGrant::Fs,
        PluginGrant::Network,
        PluginGrant::EnvPass,
        PluginGrant::OrbitTools,
        PluginGrant::Unsandboxed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fs => "fs",
            Self::Network => "network",
            Self::EnvPass => "env_pass",
            Self::OrbitTools => "orbit_tools",
            Self::Unsandboxed => "unsandboxed",
        }
    }

    /// The `--grant` spelling, or `None` for a name no surface knows.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|grant| grant.as_str() == name.trim())
    }

    /// Whether this grant can be scoped to a root list at `--grant`.
    ///
    /// Only `fs` names paths. The others are a single yes-or-no capability
    /// with nothing to narrow, so `network=loopback` is a grammar error
    /// rather than a second way to spell what the manifest already declares.
    pub fn takes_roots(self) -> bool {
        matches!(self, Self::Fs)
    }

    /// Which manifest key asks for this grant, for a diagnostic.
    pub fn requested_by(self) -> &'static str {
        match self {
            Self::Fs => "spec.permissions.fs",
            Self::Network => "spec.permissions.network",
            Self::EnvPass => "spec.permissions.env_pass",
            Self::OrbitTools => "spec.permissions.orbit_tools",
            Self::Unsandboxed => "spec.backend.sandbox: none",
        }
    }
}

impl Display for PluginGrant {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One recorded grant: the capability, plus the roots the operator scoped it
/// to when the capability takes a path scope.
///
/// `fs` is the only scoped grant today. `roots: None` is the manifest-request
/// shorthand — "whatever `spec.permissions.fs` asks for at this digest" — and
/// is what every `--grant fs` before path scoping recorded, so an existing row
/// keeps its exact meaning. `roots: Some(...)` is the operator's own list, and
/// the profile compiler grants the *intersection* of it with the manifest's
/// request, never more (design §4.1, §4.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginGrantEntry {
    pub grant: PluginGrant,
    /// The operator's roots, in the same template language as the manifest
    /// (`{{workspace}}`, `{{plugin_state}}`, absolute, or relative to the
    /// plugin root). Never empty when present.
    pub roots: Option<Vec<String>>,
}

impl PluginGrantEntry {
    /// The unscoped form: the whole request the manifest makes for this grant.
    pub fn plain(grant: PluginGrant) -> Self {
        Self { grant, roots: None }
    }

    /// The persisted and `--grant` spelling: `fs`, or `fs=<root>,<root>`.
    ///
    /// This is the string the `plugins` row stores and the grant witness
    /// hashes, so changing a root changes the recorded set and therefore the
    /// witness — re-scoping a plugin needs fresh consent exactly the way
    /// adding a grant does.
    pub fn to_recorded(&self) -> String {
        match &self.roots {
            Some(roots) => format!("{}={}", self.grant.as_str(), roots.join(",")),
            None => self.grant.as_str().to_string(),
        }
    }
}

impl Display for PluginGrantEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_recorded())
    }
}

/// An authorized grant set: at most one entry per [`PluginGrant`], in the
/// canonical [`PluginGrant::ALL`] order every surface lists them in.
///
/// Built only by the parsers below, so a set that exists is one whose grammar
/// was accepted; surfaces ask it questions (`contains`, `fs_roots`) instead of
/// comparing grant strings, which no longer match a grant name once a scope is
/// attached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginGrantSet {
    entries: Vec<PluginGrantEntry>,
}

impl PluginGrantSet {
    /// Canonicalize parsed entries: order by [`PluginGrant::ALL`], merge the
    /// roots of a repeated grant, and refuse a grant given both with and
    /// without roots — `--grant fs --grant fs=/tmp/x` asks for the whole
    /// request and a slice of it at once, and silently taking either one is
    /// how an operator ends up with a wider profile than they typed.
    pub fn from_entries(entries: Vec<PluginGrantEntry>) -> Result<Self, String> {
        let mut canonical = Vec::new();
        for grant in PluginGrant::ALL {
            let mut matching = entries
                .iter()
                .filter(|entry| entry.grant == grant)
                .peekable();
            if matching.peek().is_none() {
                continue;
            }
            let mut roots: Vec<String> = Vec::new();
            let mut saw_plain = false;
            let mut saw_scoped = false;
            for entry in matching {
                match &entry.roots {
                    Some(scoped) => {
                        saw_scoped = true;
                        for root in scoped {
                            if !roots.iter().any(|seen| seen == root) {
                                roots.push(root.clone());
                            }
                        }
                    }
                    None => saw_plain = true,
                }
            }
            if saw_plain && saw_scoped {
                return Err(format!(
                    "grant `{grant}` was given both with and without roots; write \
                     `{grant}=<root>[,<root>]` to scope it, or `{grant}` on its own to grant \
                     every root the manifest requests"
                ));
            }
            canonical.push(PluginGrantEntry {
                grant,
                roots: saw_scoped.then_some(roots),
            });
        }
        Ok(Self { entries: canonical })
    }

    /// The unscoped set of `grants`: every one of them as the whole request
    /// the manifest makes. Infallible, because plain entries cannot conflict
    /// the way [`Self::from_entries`] guards against.
    pub fn from_grants(grants: impl IntoIterator<Item = PluginGrant>) -> Self {
        let entries = grants.into_iter().map(PluginGrantEntry::plain).collect();
        // The only error `from_entries` returns needs a scoped entry.
        Self::from_entries(entries).unwrap_or_default()
    }

    pub fn contains(&self, grant: PluginGrant) -> bool {
        self.entries.iter().any(|entry| entry.grant == grant)
    }

    pub fn entry(&self, grant: PluginGrant) -> Option<&PluginGrantEntry> {
        self.entries.iter().find(|entry| entry.grant == grant)
    }

    /// The roots `fs` was scoped to, or `None` for the manifest-request
    /// shorthand *and* for a set that does not hold `fs` at all — callers ask
    /// [`Self::contains`] first, because "no scope" and "no grant" are
    /// different answers.
    pub fn fs_roots(&self) -> Option<&[String]> {
        self.entry(PluginGrant::Fs)
            .and_then(|entry| entry.roots.as_deref())
    }

    /// The capabilities held, without their scopes.
    pub fn grants(&self) -> Vec<PluginGrant> {
        self.entries.iter().map(|entry| entry.grant).collect()
    }

    /// The set as the `plugins` row records it and the witness hashes it.
    pub fn to_recorded(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(PluginGrantEntry::to_recorded)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, PluginGrantEntry> {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a PluginGrantSet {
    type Item = &'a PluginGrantEntry;
    type IntoIter = std::slice::Iter<'a, PluginGrantEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

/// Whether a root that was *not* attached to its grant with `=` can be read as
/// a path rather than a mistyped grant name.
///
/// `--grant fs=/srv/data,/srv/cache` has to keep reading the second segment as
/// a root, because comma is both the grant separator and the root separator
/// (design §4.1). Without a rule, `--grant fs=/srv/data,netwrok` would record
/// a typo as a filesystem root and say nothing. So a continuation root must
/// look like a path — absolute, explicitly relative, home-relative, or a
/// template — and anything else is reported as the unknown grant it probably
/// is. A bare relative root is still grantable; after the first it is written
/// `./<root>`, the same way the shell distinguishes one from a command name.
fn looks_like_root(segment: &str) -> bool {
    segment.starts_with('/')
        || segment.starts_with("./")
        || segment.starts_with("../")
        || segment.starts_with('~')
        || segment.starts_with("{{")
}

/// Parse a `--grant` or stored grant list into entries, in the order written.
///
/// Each value is split on commas, so `fs,network`, `--grant fs --grant
/// network` and a stored `["fs=/a,/b", "network"]` row all parse the same way.
pub fn parse_grant_entries(values: &[String]) -> Result<Vec<PluginGrantEntry>, String> {
    let mut entries: Vec<PluginGrantEntry> = Vec::new();
    let mut unknown = Vec::new();
    // Index of the scoped entry still collecting roots: every following
    // segment that is a path and not a grant name belongs to it.
    let mut open: Option<usize> = None;
    for raw in values.iter().flat_map(|value| value.split(',')) {
        let segment = raw.trim();
        if segment.is_empty() {
            continue;
        }
        if let Some((name, first_root)) = segment.split_once('=') {
            open = None;
            let Some(grant) = PluginGrant::parse(name) else {
                unknown.push(name.trim().to_string());
                continue;
            };
            if !grant.takes_roots() {
                return Err(format!(
                    "grant `{grant}` does not take roots; write `{grant}` on its own"
                ));
            }
            let first_root = first_root.trim();
            if first_root.is_empty() {
                return Err(format!(
                    "`{grant}=` needs at least one root; write `{grant}` on its own to grant \
                     every root the manifest requests"
                ));
            }
            entries.push(PluginGrantEntry {
                grant,
                roots: Some(vec![first_root.to_string()]),
            });
            open = Some(entries.len() - 1);
        } else if let Some(grant) = PluginGrant::parse(segment) {
            open = None;
            entries.push(PluginGrantEntry::plain(grant));
        } else if let Some(index) = open.filter(|_| looks_like_root(segment)) {
            entries[index]
                .roots
                .get_or_insert_with(Vec::new)
                .push(segment.to_string());
        } else {
            open = None;
            unknown.push(segment.to_string());
        }
    }
    if !unknown.is_empty() {
        return Err(format!(
            "unknown grant{} {}; valid grants are {}",
            if unknown.len() == 1 { "" } else { "s" },
            unknown.join(", "),
            PluginGrant::ALL
                .iter()
                .map(|grant| grant.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(entries)
}

/// Validate a `--grant` list: every name must be one of [`PluginGrant::ALL`],
/// and `fs` may carry the roots it is scoped to.
pub fn parse_grants(names: &[String]) -> Result<PluginGrantSet, String> {
    PluginGrantSet::from_entries(parse_grant_entries(names)?)
}

/// Parse a `plugins` row's persisted grant strings into the typed set the
/// runtime builds a plugin's backend from.
///
/// Unlike [`parse_grants`] — which validates a `--grant` list an operator just
/// typed — this runs on grants already recorded in storage: an unknown name
/// here means the row is corrupt, or was written by a newer Orbit that
/// renamed or retired a grant this build does not know about. Dropping it
/// silently would run the plugin under fewer grants than were authorized
/// without saying so; the caller must refuse the row instead (design §4.1).
pub fn parse_stored_grants(names: &[String]) -> Result<PluginGrantSet, String> {
    parse_grants(names)
}

/// Resolve a `--grant` value list into the grants to record, including its
/// three reserved spellings: a lone `none` records an explicit empty set —
/// the CLI's way to revoke every grant, since an empty list otherwise means
/// "leave the recorded grants alone" (design §4.1's narrower-list revocation
/// otherwise stops one short of empty); a lone `all` is every grant this
/// build knows; a lone `requested` is exactly what `manifest` asks for. All
/// three record the unscoped form of every grant they select, because none of
/// them names a root. Any other list is validated the way an ordinary
/// `--grant` list always was, so none of the three names can be mixed into a
/// literal grant list.
pub fn resolve_grant_selection(
    names: &[String],
    manifest: &PluginManifest,
) -> Result<PluginGrantSet, String> {
    let selected = match names {
        [only] if only.trim() == "none" => Vec::new(),
        [only] if only.trim() == "all" => PluginGrant::ALL.to_vec(),
        [only] if only.trim() == "requested" => manifest.required_grants(),
        _ => return parse_grants(names),
    };
    PluginGrantSet::from_entries(selected.into_iter().map(PluginGrantEntry::plain).collect())
}

/// What one grant means for this manifest: whether the manifest asks for it,
/// and the concrete request behind the ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginGrantRequest {
    pub grant: PluginGrant,
    /// The request as the manifest states it (`read=…, write=…`), empty when
    /// the manifest does not ask for this grant.
    pub requested: Option<String>,
}

impl PluginManifest {
    /// Every grant with the manifest's request beside it, in canonical order.
    pub fn grant_requests(&self) -> Vec<PluginGrantRequest> {
        let permissions = &self.spec.permissions;
        PluginGrant::ALL
            .into_iter()
            .map(|grant| {
                let requested = match grant {
                    PluginGrant::Fs => {
                        let mut parts = Vec::new();
                        if !permissions.fs.read.is_empty() {
                            parts.push(format!("read={}", permissions.fs.read.join(",")));
                        }
                        if !permissions.fs.write.is_empty() {
                            parts.push(format!("write={}", permissions.fs.write.join(",")));
                        }
                        (!parts.is_empty()).then(|| parts.join(" "))
                    }
                    PluginGrant::Network => match permissions.network {
                        PluginNetworkPermission::None => None,
                        PluginNetworkPermission::Loopback => Some("loopback".to_string()),
                        PluginNetworkPermission::Any => Some("any".to_string()),
                    },
                    PluginGrant::EnvPass => {
                        (!permissions.env_pass.is_empty()).then(|| permissions.env_pass.join(","))
                    }
                    PluginGrant::OrbitTools => (!permissions.orbit_tools.is_empty())
                        .then(|| permissions.orbit_tools.join(",")),
                    PluginGrant::Unsandboxed => (self.spec.backend.sandbox == PluginSandbox::None)
                        .then(|| "backend.sandbox: none".to_string()),
                };
                PluginGrantRequest { grant, requested }
            })
            .collect()
    }

    /// The grants this manifest cannot run without.
    pub fn required_grants(&self) -> Vec<PluginGrant> {
        self.grant_requests()
            .into_iter()
            .filter(|request| request.requested.is_some())
            .map(|request| request.grant)
            .collect()
    }

    /// Required grants the operator has not recorded, in canonical order.
    ///
    /// `granted` is the row's persisted strings, which carry a scope for a
    /// path-scoped grant (`fs=/srv/data`), so this compares parsed
    /// capabilities rather than spellings. A row this build cannot parse is
    /// read as granting nothing here; the loader refuses it separately with a
    /// diagnostic that names the grant it could not read.
    pub fn missing_grants(&self, granted: &[String]) -> Vec<PluginGrant> {
        let granted = parse_stored_grants(granted).unwrap_or_default();
        self.required_grants()
            .into_iter()
            .filter(|grant| !granted.contains(*grant))
            .collect()
    }
}
