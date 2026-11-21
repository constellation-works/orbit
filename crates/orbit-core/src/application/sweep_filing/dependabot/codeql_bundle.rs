//! Whether the owner host's installed CodeQL bundle can confirm a repair.
//!
//! A code-scanning repair task asks its implementer to confirm the fix with
//! the repository's local CodeQL procedure, on the owner host. Hosted CodeQL
//! floats with the runner's bundle while a host keeps the bundle it has, so a
//! query new in the hosted bundle reached the backlog as a repair task the
//! host could never confirm: it spent a drain slot and blocked
//! `validation_unavailable` (ORB-26147, `rust/unsafe-deserialization`, new in
//! CodeQL 2.27.5 while the host ran 2.27.1).
//!
//! At filing the sweep therefore asks the installed bundle which rule ids it
//! offers. A rule it offers files as before. A rule it lacks, or any rule when
//! no bundle is installed, still files its repair task, but held behind one
//! prerequisite per host and hosted bundle version: the repair depends on it,
//! so no drain admits the repair until an operator installs the bundle and
//! closes the prerequisite. The drift itself is raised once as an open
//! friction record, which operator attention reports.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::OrbitId;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::Value;

use super::filing::field;
use crate::OrbitRuntime;
use crate::application::sweep_filing::{digest, display};
use crate::application::task::TaskAddParams;

/// Tag every bundle prerequisite carries.
pub(super) const CODEQL_PREREQUISITE_TAG: &str = "codeql-bundle-prerequisite";
/// Tag prefix of a prerequisite's host and hosted-version identity.
const CODEQL_PREREQUISITE_KEY_PREFIX: &str = "codeql-bundle-prerequisite:";
/// One `codeql` invocation: `version` answers in about a second and a suite
/// resolves in about the same, so this only bounds a wedged CLI.
const CODEQL_PROBE_TIMEOUT: Duration = Duration::from_secs(120);
/// Captured output kept per stream; a resolved suite lists a few hundred paths.
const CODEQL_PROBE_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
/// Bytes of a query file read for its `@id`; metadata leads the file.
const QUERY_HEADER_LIMIT: usize = 16 * 1024;
/// Friction tags in preference order; the taxonomy may know only some.
const FRICTION_TAGS: &[&str] = &["tooling", "other"];

/// The installed bundle as the filing host sees it.
pub(super) struct LocalCodeqlBundle {
    /// Display name of the owner host.
    pub(super) host: String,
    /// `codeql version`, or `None` when no CLI answers in the owner's
    /// validation environment.
    pub(super) version: Option<String>,
    env: Vec<(String, String)>,
    /// Rule ids each query pack offers, resolved once per sweep.
    rules_by_pack: BTreeMap<String, BTreeSet<String>>,
}

/// Why a code-scanning group is held instead of filed as drainable work.
#[derive(Clone)]
pub(super) struct BundleHold {
    pub(super) host: String,
    pub(super) hosted_version: String,
    pub(super) local_version: Option<String>,
    pub(super) rule_id: String,
}

impl BundleHold {
    /// One prerequisite per host and hosted bundle version.
    pub(super) fn prerequisite_key(&self) -> String {
        digest(&["codeql-bundle", &self.host, &self.hosted_version])
    }

    fn local_display(&self) -> String {
        self.local_version.as_deref().map_or_else(
            || "none installed".to_string(),
            |version| version.to_string(),
        )
    }
}

impl LocalCodeqlBundle {
    /// Ask the owner host's CodeQL CLI for its version. Runs in the same
    /// environment owner-side repository tooling uses, so a sweep started by
    /// a service manager with a minimal PATH still finds the bundle.
    pub(super) fn probe(runtime: &OrbitRuntime) -> Self {
        let env = runtime.validation_environment().env;
        let version = run_codeql(&env, &["version", "--format=json"])
            .and_then(|stdout| serde_json::from_slice::<Value>(&stdout).ok())
            .and_then(|value| {
                value
                    .get("version")
                    .and_then(Value::as_str)
                    .map(|version| version.trim().to_string())
            })
            .filter(|version| !version.is_empty());
        Self {
            host: owner_host_name(runtime),
            version,
            env,
            rules_by_pack: BTreeMap::new(),
        }
    }

    /// `None` when the installed bundle offers the alert's rule, else the
    /// hold to file the group behind. Only CodeQL alerts are checked: other
    /// tools are not confirmed with the local CodeQL procedure.
    pub(super) fn hold_for(&mut self, alert: &Value) -> Option<BundleHold> {
        if !field(alert, "tool_name").eq_ignore_ascii_case("codeql") {
            return None;
        }
        let rule_id = field(alert, "rule_id");
        if rule_id.is_empty() || self.offers(&rule_id) {
            return None;
        }
        let hosted_version = field(alert, "tool_version");
        Some(BundleHold {
            host: self.host.clone(),
            hosted_version: if hosted_version.is_empty() {
                "unknown".to_string()
            } else {
                hosted_version
            },
            local_version: self.version.clone(),
            rule_id,
        })
    }

    fn offers(&mut self, rule_id: &str) -> bool {
        if self.version.is_none() {
            return false;
        }
        let Some(pack) = rule_id
            .split_once('/')
            .and_then(|(prefix, _)| pack_for(prefix))
        else {
            return false;
        };
        if !self.rules_by_pack.contains_key(pack) {
            let rules = self.resolve_pack_rules(pack);
            self.rules_by_pack.insert(pack.to_string(), rules);
        }
        self.rules_by_pack
            .get(pack)
            .is_some_and(|rules| rules.contains(rule_id))
    }

    /// Every rule id in the pack's widest standard suite. Hosted default
    /// analysis runs a subset of it, so a rule missing here is missing from
    /// any local run of the bundle. A failed resolution offers nothing.
    fn resolve_pack_rules(&self, pack: &str) -> BTreeSet<String> {
        let suite = format!("codeql/{pack}-queries:codeql-suites/{pack}-security-and-quality.qls");
        let Some(stdout) = run_codeql(
            &self.env,
            &["resolve", "queries", "--format=json", suite.as_str()],
        ) else {
            return BTreeSet::new();
        };
        let Ok(paths) = serde_json::from_slice::<Vec<String>>(&stdout) else {
            return BTreeSet::new();
        };
        paths
            .iter()
            .filter_map(|path| query_rule_id(Path::new(path)))
            .collect()
    }
}

/// CodeQL query pack of a rule-id language prefix.
fn pack_for(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "rust" => "rust",
        "js" => "javascript",
        "actions" => "actions",
        "py" => "python",
        "java" => "java",
        "cpp" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "rb" => "ruby",
        "swift" => "swift",
        _ => return None,
    })
}

fn run_codeql(env: &[(String, String)], args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("codeql");
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)));
    let output = orbit_common::process::run_bounded_capped(
        &mut command,
        CODEQL_PROBE_TIMEOUT,
        CODEQL_PROBE_OUTPUT_LIMIT,
    )
    .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The `@id` in a query's metadata comment.
fn query_rule_id(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut header = Vec::new();
    file.take(QUERY_HEADER_LIMIT as u64)
        .read_to_end(&mut header)
        .ok()?;
    String::from_utf8_lossy(&header).lines().find_map(|line| {
        let rest = line.trim_start().trim_start_matches('*').trim_start();
        let id = rest.strip_prefix("@id")?;
        // `@idfoo` is another tag.
        if !id.starts_with(char::is_whitespace) {
            return None;
        }
        id.split_whitespace().next().map(str::to_string)
    })
}

/// The owner host's display name, as operator attention names it.
fn owner_host_name(runtime: &OrbitRuntime) -> String {
    runtime
        .automation_execution_location()
        .and_then(|location| location.machine_name.clone())
        .or_else(|| {
            orbit_config::load_machine_settings(&runtime.global_root())
                .ok()
                .and_then(|machine| machine.name)
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

/// The open prerequisite already holding repairs for `key`, if any.
pub(super) fn open_prerequisite(
    runtime: &OrbitRuntime,
    key: &str,
) -> Result<Option<OrbitId>, OrbitError> {
    let tag = format!("{CODEQL_PREREQUISITE_KEY_PREFIX}{key}");
    Ok(runtime
        .list_tasks()?
        .into_iter()
        .filter(|task| !task.status.is_terminal())
        .filter(|task| task.tags.iter().any(|candidate| candidate == &tag))
        .min_by(|left, right| left.created_at.cmp(&right.created_at))
        .map(|task| task.id))
}

/// The operator task a held repair waits on. It is proposed, awaiting a
/// person: installing a bundle on a host is not agent work.
pub(super) fn prerequisite_task_params(
    hold: &BundleHold,
    rules: &BTreeSet<String>,
) -> TaskAddParams {
    let key = hold.prerequisite_key();
    let rules = rules
        .iter()
        .map(|rule| format!("`{}`", display(rule)))
        .collect::<Vec<_>>()
        .join(", ");
    let description = format!(
        "Hosted CodeQL analysed this repository with CodeQL bundle `{hosted}`, and its alerts include rules the CodeQL bundle installed on `{host}` ({local}) does not offer: {rules}. A repair for such a rule cannot be confirmed with the repository's documented local CodeQL procedure on `{host}`, so its repair task depends on this task instead of reaching a drain.\n\n\
         ## Bundle drift\n\n\
         - Host: `{host}`\n\
         - Hosted CodeQL bundle: `{hosted}`\n\
         - Installed CodeQL bundle: `{local}`\n\
         - Rules missing locally: {rules}\n\n\
         Install CodeQL bundle `{hosted}` (CLI plus its query packs) on `{host}`, and make the extractor toolchain that bundle requests available to the local CodeQL procedure: a Rust extractor names the Rust toolchain version it needs, and the procedure must be given that version rather than the one confirmed for the older bundle. Then mark this task done; every repair waiting on it becomes eligible.\n",
        hosted = display(&hold.hosted_version),
        host = display(&hold.host),
        local = hold.local_display(),
    );
    TaskAddParams {
        title: format!(
            "CodeQL bundle {} needed on {}",
            display(&hold.hosted_version),
            display(&hold.host)
        ),
        description,
        acceptance_criteria: vec![
            format!(
                "`codeql version` on `{}` reports CodeQL bundle `{}` or newer.",
                display(&hold.host),
                display(&hold.hosted_version)
            ),
            "`codeql resolve queries` on the installed bundle offers the rule of every code-scanning repair task that depends on this task.".to_string(),
            "The extractor toolchain the installed bundle requests is recorded and available to the repository's documented local CodeQL procedure.".to_string(),
        ],
        tags: vec![
            CODEQL_PREREQUISITE_TAG.to_string(),
            format!("{CODEQL_PREREQUISITE_KEY_PREFIX}{key}"),
        ],
        priority: TaskPriority::High,
        complexity: TaskComplexity::Unassessed,
        task_type: Some(TaskType::Chore),
        status: Some(TaskStatus::Proposed),
        system_created: true,
        ..TaskAddParams::default()
    }
}

/// Raise the drift once per host, installed and hosted version, as an open
/// friction record that operator attention lists. Answers its id.
pub(super) fn report_bundle_drift(
    runtime: &OrbitRuntime,
    hold: &BundleHold,
    prerequisite_id: &str,
    rules: &BTreeSet<String>,
) -> Result<String, OrbitError> {
    let local = hold.local_display();
    let key = format!(
        "codeql-bundle-drift:{}",
        digest(&[
            "codeql-bundle-drift",
            &hold.host,
            &local,
            &hold.hosted_version
        ])
    );
    let rules = rules
        .iter()
        .map(|rule| format!("`{}`", display(rule)))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        "Hosted CodeQL runs bundle `{hosted}`, but `{host}` has `{local}`. The code-scanning sweep held repairs for rules the installed bundle does not offer ({rules}) behind {prerequisite_id} instead of filing them as drainable work.\n\n\
         - host: `{host}`\n\
         - hosted bundle: `{hosted}`\n\
         - installed bundle: `{local}`\n\n\
         Install the hosted bundle on `{host}` and close {prerequisite_id}.\n\
         dedupe-key: {key}\n",
        hosted = display(&hold.hosted_version),
        host = display(&hold.host),
    );
    let frictions = crate::runtime::friction::store_for(runtime)?;
    let known = frictions.tags()?;
    let tags = FRICTION_TAGS
        .iter()
        .find(|tag| known.iter().any(|candidate| candidate == *tag))
        .map(|tag| vec![(*tag).to_string()])
        .unwrap_or_default();
    let stored = frictions.add_or_reuse(
        &key,
        FrictionAddParams {
            model: "system".to_string(),
            title: Some(format!(
                "CodeQL bundle drift on {}: hosted {}, installed {local}",
                display(&hold.host),
                display(&hold.hosted_version),
            )),
            body,
            tags,
            status: FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        },
    )?;
    Ok(stored.record.id)
}

/// The description section a held repair carries.
pub(super) fn hold_section(hold: &BundleHold) -> String {
    format!(
        "\n## Waiting on the local CodeQL bundle\n\nHosted CodeQL `{}` reported rule `{}`, which the CodeQL bundle installed on `{}` ({}) does not offer, so the local CodeQL procedure cannot confirm this repair there yet. This task depends on the CodeQL bundle prerequisite for that host and version and stays out of drains until it is done.\n",
        display(&hold.hosted_version),
        display(&hold.rule_id),
        display(&hold.host),
        hold.local_display(),
    )
}
