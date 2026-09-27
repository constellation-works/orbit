//! The profile of a backend the host spawns on an agent's behalf (design
//! `docs/design/plugins/2_agent_call_broker.md` §5).
//!
//! A backend spawned inside the agent sandbox inherits the agent's
//! restrictions. One the host broker spawns does not, so its profile is
//! compiled from both sides: the plugin profile [`PluginBackendSpec::sandbox_profile`]
//! builds, narrowed by the calling run's resolved filesystem profile. The
//! plugin's own `{{plugin_state}}` is the single path that may exceed the
//! agent profile. The non-brokered profile is untouched.

use regex::Regex;

use super::*;

/// The agent run a brokered call is made for, as the host's own dispatch
/// record states it. Nothing here is read from the request.
#[derive(Debug, Clone)]
pub struct BrokeredCaller {
    /// The run's worktree. `{{workspace}}` renders here, and the profile's
    /// workspace-relative rules are anchored here.
    pub worktree: PathBuf,
    /// The filesystem profile the agent was sandboxed with, including the
    /// absolute runtime write roots the host appended for its nested `orbit`.
    pub fs_profile: ResolvedFsProfile,
    /// The run's `proc.spawn` allowlist. An empty list denies every program.
    pub proc_allowed_programs: Vec<String>,
    /// The run's program disallow list when its activity selects program deny
    /// mode; it then decides in place of the allowlist, as for `proc.spawn`.
    pub proc_disallowed_programs: Option<Vec<String>>,
}

impl BrokeredCaller {
    /// Narrow `ctx` to this caller: its worktree and its `proc.spawn`
    /// program policy, held as an activity-scoped caller's so an empty
    /// allowlist denies every program rather than allowing them all.
    pub fn restrict(&self, ctx: &mut ToolContext) {
        ctx.workspace_root = Some(self.worktree.clone());
        ctx.proc_allowed_programs = self.proc_allowed_programs.clone();
        ctx.proc_disallowed_programs = self.proc_disallowed_programs.clone();
        ctx.proc_spawn_activity_scoped = true;
    }
}

impl PluginBackendSpec {
    /// The boundary for a backend the host spawns on `caller`'s behalf: the
    /// plugin profile ∩ the caller's filesystem profile.
    ///
    /// - A write root or file survives only where the caller's `modify` rules,
    ///   evaluated last-match-wins, allow it, and only when no `modify`
    ///   exclusion names a path already beneath it: a kernel write grant
    ///   cannot carve one out. Anything dropped is logged.
    /// - `{{plugin_state}}` is exempt. It stays readable and, with an
    ///   `fs.write` grant, writable, although the agent profile does not
    ///   reach it.
    /// - A read root at or beneath a caller read exclusion or a default
    ///   credential location is dropped; the exclusions themselves and the
    ///   credential locations are denied beneath every root that remains.
    /// - Every declared program must be on the caller's `proc.spawn`
    ///   allowlist.
    ///
    /// A rule the caller's profile negates is treated as a deny even where a
    /// later rule re-allows part of it: that costs the backend access, never
    /// widens it.
    pub fn brokered_sandbox_profile(
        &self,
        caller: &BrokeredCaller,
    ) -> Result<PluginSandboxProfile, OrbitError> {
        let mut ctx = ToolContext::default();
        caller.restrict(&mut ctx);
        self.enforce_programs(&ctx, &self.provenance.name)?;

        let mut profile = self.sandbox_profile(Some(&caller.worktree))?;
        let worktree = physical_with_missing_tail(&caller.worktree);
        let read_rules = CallerRules::anchor(&worktree, &caller.fs_profile.read)?;
        let modify_rules = CallerRules::anchor(&worktree, &caller.fs_profile.modify)?;
        let state_dir = physical_with_missing_tail(&profile.state_dir);
        let own_state = |path: &Path| physical_with_missing_tail(path).starts_with(&state_dir);

        let credentials = orbit_exec::default_credential_read_denies();
        profile.read.retain(|root| {
            if own_state(root) {
                return true;
            }
            let path = physical_with_missing_tail(root);
            let denied = read_rules.excludes_at_or_above(&path)
                || credentials
                    .iter()
                    .any(|credential| path.starts_with(physical_with_missing_tail(credential)));
            if denied {
                self.report_dropped_root(root, "read", "the calling agent may not read it");
            }
            !denied
        });
        for credential in credentials {
            if !profile.read_denies.contains(&credential) {
                profile.read_denies.push(credential);
            }
        }

        let existing_exclusions = modify_rules.existing_wildcard_matches()?;
        let write = std::mem::take(&mut profile.write);
        for root in write {
            if own_state(&root) {
                profile.write.push(root);
                continue;
            }
            let path = physical_with_missing_tail(&root);
            let refusal = if !modify_rules.allows(&path) {
                Some("the calling agent may not write it")
            } else if modify_rules.literal_exclusions_beneath(&path)
                || existing_exclusions
                    .iter()
                    .any(|excluded| excluded.starts_with(&path))
            {
                Some("the calling agent may not write a path beneath it")
            } else {
                None
            };
            match refusal {
                Some(reason) => self.report_dropped_root(&root, "write", reason),
                None => profile.write.push(root),
            }
        }
        let write_files = std::mem::take(&mut profile.write_files);
        for file in write_files {
            if own_state(&file) || modify_rules.allows(&physical_with_missing_tail(&file)) {
                profile.write_files.push(file);
            } else {
                self.report_dropped_root(&file, "write", "the calling agent may not write it");
            }
        }
        let unheld: Vec<&str> = modify_rules
            .wildcard_exclusions()
            .filter(|pattern| {
                profile
                    .write
                    .iter()
                    .any(|root| !own_state(root) && reaches(pattern, root))
            })
            .collect();
        if cfg!(target_os = "linux") && !unheld.is_empty() {
            tracing::warn!(
                target: "orbit.tools.plugin",
                plugin = %self.provenance.name,
                exclusions = unheld.join(" ").as_str(),
                "the calling agent's write exclusions can name paths inside a granted write root; \
                 Landlock cannot refuse a name they match that is created after spawn",
            );
        }

        profile.caller_read_exclusions = read_rules.exclusion_patterns();
        let (own, kept): (Vec<&PathBuf>, Vec<&PathBuf>) =
            profile.write.iter().partition(|root| own_state(root));
        let mut write_rules = modify_rules.seatbelt_write_rules(&kept, &profile.write_files);
        // `{{plugin_state}}` is the one widening, so it is re-allowed after
        // every caller rule rather than left to their order.
        write_rules.extend(own.into_iter().map(|root| subtree_rule(root)));
        profile.caller_write_rules = write_rules;
        Ok(profile)
    }

    fn report_dropped_root(&self, root: &Path, access: &str, reason: &str) {
        tracing::warn!(
            target: "orbit.tools.plugin",
            plugin = %self.provenance.name,
            access,
            root = %root.display(),
            reason,
            "a granted filesystem root is outside the calling agent's profile; it is not on the \
             brokered backend's sandbox profile",
        );
    }
}

/// One side of a caller's profile, every rule anchored to an absolute path so
/// it can be decided against the backend's absolute roots.
struct CallerRules {
    rules: Vec<CallerRule>,
}

struct CallerRule {
    negated: bool,
    /// The absolute pattern, in the profile's glob grammar.
    pattern: String,
    matcher: Regex,
}

impl CallerRules {
    /// Anchor `rules` at `worktree`. An absolute rule — a runtime root the
    /// host appended — is kept as written.
    fn anchor(worktree: &Path, rules: &[String]) -> Result<Self, OrbitError> {
        let worktree = worktree.to_string_lossy().replace('\\', "/");
        let worktree = worktree.trim_end_matches('/');
        let rules = rules
            .iter()
            .map(|rule| {
                let (negated, body) = rule
                    .strip_prefix('!')
                    .map_or((false, rule.as_str()), |body| (true, body));
                let mut body = body.replace('\\', "/");
                while let Some(stripped) = body.strip_prefix("./") {
                    body = stripped.to_string();
                }
                let pattern = if body.starts_with('/') {
                    body
                } else if body.is_empty() || body == "." {
                    worktree.to_string()
                } else {
                    format!("{worktree}/{body}")
                };
                let matcher = orbit_types::policy::compile_glob_regex(&pattern).map_err(|error| {
                    OrbitError::PolicyDenied(format!(
                        "the calling agent's filesystem rule `{rule}` is not a valid glob: {error}"
                    ))
                })?;
                Ok(CallerRule {
                    negated,
                    pattern,
                    matcher,
                })
            })
            .collect::<Result<_, OrbitError>>()?;
        Ok(Self { rules })
    }

    /// Last matching rule wins; a path no rule matches is not allowed.
    fn allows(&self, path: &Path) -> bool {
        let path = path.to_string_lossy().replace('\\', "/");
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matcher.is_match(&path))
            .is_some_and(|rule| !rule.negated)
    }

    fn exclusions(&self) -> impl Iterator<Item = &CallerRule> {
        self.rules.iter().filter(|rule| rule.negated)
    }

    fn exclusion_patterns(&self) -> Vec<String> {
        self.exclusions().map(|rule| rule.pattern.clone()).collect()
    }

    /// These rules replayed in order after the plugin's own write grants, for
    /// a profile that is last-match-wins: every exclusion as written, and
    /// every grant clipped to the write roots and files the backend kept.
    ///
    /// Replaying the exclusions alone would deny a kept root the caller
    /// re-allows beneath one (`.orbit/tmp/**` under `.orbit/**`); replaying
    /// the grants unclipped would hand the backend the agent's whole write
    /// set. A wildcard grant whose literal prefix sits above a kept root has
    /// no clipped form and is left out, which only narrows.
    fn seatbelt_write_rules(&self, roots: &[&PathBuf], files: &[PathBuf]) -> Vec<String> {
        let mut out = Vec::new();
        let mut excluded = false;
        for rule in &self.rules {
            if rule.negated {
                out.push(format!("!{}", rule.pattern));
                excluded = true;
                continue;
            }
            // A grant before any exclusion has nothing to re-allow.
            if !excluded {
                continue;
            }
            for root in roots {
                let root = physical_with_missing_tail(root);
                if covers(&rule.pattern, &root) {
                    out.push(subtree_rule(&root));
                } else if within(&rule.pattern, &root) {
                    out.push(rule.pattern.clone());
                }
            }
            for file in files {
                let file = physical_with_missing_tail(file);
                if rule
                    .matcher
                    .is_match(&file.to_string_lossy().replace('\\', "/"))
                {
                    out.push(file.to_string_lossy().into_owned());
                }
            }
        }
        out
    }

    fn wildcard_exclusions(&self) -> impl Iterator<Item = &str> {
        self.exclusions()
            .map(|rule| rule.pattern.as_str())
            .filter(|pattern| literal_root(pattern).is_none())
    }

    /// Whether an exclusion names `path` or one of its ancestors: a read
    /// root there would hand the backend what the agent may not read.
    fn excludes_at_or_above(&self, path: &Path) -> bool {
        path.ancestors().any(|ancestor| {
            let ancestor = ancestor.to_string_lossy().replace('\\', "/");
            self.exclusions()
                .any(|rule| rule.matcher.is_match(&ancestor))
        })
    }

    /// Whether an exact or subtree exclusion names a path at or beneath
    /// `root`, present or not.
    fn literal_exclusions_beneath(&self, root: &Path) -> bool {
        self.exclusions()
            .filter_map(|rule| literal_root(&rule.pattern))
            .any(|excluded| physical_with_missing_tail(&excluded).starts_with(root))
    }

    /// The paths the wildcard exclusions match now.
    fn existing_wildcard_matches(&self) -> Result<Vec<PathBuf>, OrbitError> {
        let wildcard: Vec<String> = self.wildcard_exclusions().map(str::to_string).collect();
        if wildcard.is_empty() {
            return Ok(Vec::new());
        }
        Ok(orbit_exec::existing_glob_matches(&wildcard)?
            .into_iter()
            .collect())
    }
}

/// The path an exact or `<path>/**` pattern names; `None` for a pattern with
/// a wildcard anywhere else.
fn literal_root(pattern: &str) -> Option<PathBuf> {
    let body = pattern.strip_suffix("/**").unwrap_or(pattern);
    (!body.contains(['*', '?'])).then(|| PathBuf::from(body))
}

/// Whether `pattern` grants everything at and beneath `root`.
fn covers(pattern: &str, root: &Path) -> bool {
    match pattern.strip_suffix("/**") {
        Some(prefix) if !prefix.contains(['*', '?']) => root.starts_with(prefix),
        Some(_) | None => literal_root(pattern).is_some_and(|exact| exact == root),
    }
}

/// Whether everything `pattern` names lies beneath `root`.
fn within(pattern: &str, root: &Path) -> bool {
    let wildcard = pattern.find(['*', '?']).unwrap_or(pattern.len());
    Path::new(&pattern[..wildcard]).starts_with(root)
}

/// A directory as a subtree rule in the profile grammar.
fn subtree_rule(root: &Path) -> String {
    format!("{}/**", physical_with_missing_tail(root).display())
}

/// Whether a wildcard `pattern` can name a path beneath `root`: its literal
/// prefix and the root share a line of descent.
fn reaches(pattern: &str, root: &Path) -> bool {
    let wildcard = pattern.find(['*', '?']).unwrap_or(pattern.len());
    let literal = &pattern[..wildcard];
    let prefix = Path::new(&literal[..literal.rfind('/').unwrap_or(0).max(1)]);
    let root = physical_with_missing_tail(root);
    root.starts_with(prefix) || prefix.starts_with(&root)
}
