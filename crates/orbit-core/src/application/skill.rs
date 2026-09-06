use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::skill_catalog::{LoadedSkill, SkillCatalogDoctorStatus};

use super::{ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets};

/// Every shipped skill file, keyed by its path relative to the skills root.
///
/// Skills are directory trees rather than single documents, so their managed
/// manifest keys on the relative path ([`ManagedAssetLayout::RelativePath`])
/// instead of a bare definition name.
///
/// Each shipped skill's `SKILL.md` is a router carrying only what every call
/// needs; its own references load on demand, so a skill's whole surface stays
/// discoverable through one description without a per-topic skill competing
/// for the same trigger. The ordering below mirrors each router's own
/// reference table, so the shipped surface reads the same in both places.
pub(crate) const DEFAULT_SKILL_FILES: [(&str, &str); 28] = [
    (
        "orbit/SKILL.md",
        include_str!("../../assets/skills/orbit/SKILL.md"),
    ),
    // The vocabulary, then working through Orbit.
    (
        "orbit/references/concepts.md",
        include_str!("../../assets/skills/orbit/references/concepts.md"),
    ),
    (
        "orbit/references/tool-surface.md",
        include_str!("../../assets/skills/orbit/references/tool-surface.md"),
    ),
    (
        "orbit/references/task-authoring.md",
        include_str!("../../assets/skills/orbit/references/task-authoring.md"),
    ),
    (
        "orbit/references/task-execution.md",
        include_str!("../../assets/skills/orbit/references/task-execution.md"),
    ),
    (
        "orbit/references/task-review.md",
        include_str!("../../assets/skills/orbit/references/task-review.md"),
    ),
    (
        "orbit/references/search.md",
        include_str!("../../assets/skills/orbit/references/search.md"),
    ),
    (
        "orbit/references/docs-corpus.md",
        include_str!("../../assets/skills/orbit/references/docs-corpus.md"),
    ),
    (
        "orbit/references/friction.md",
        include_str!("../../assets/skills/orbit/references/friction.md"),
    ),
    (
        "orbit/references/orchestration.md",
        include_str!("../../assets/skills/orbit/references/orchestration.md"),
    ),
    (
        "orbit/references/workflows.md",
        include_str!("../../assets/skills/orbit/references/workflows.md"),
    ),
    (
        "orbit/references/run-debugging.md",
        include_str!("../../assets/skills/orbit/references/run-debugging.md"),
    ),
    (
        "orbit/references/common-failures.md",
        include_str!("../../assets/skills/orbit/references/common-failures.md"),
    ),
    (
        "orbit/references/operational-logs.md",
        include_str!("../../assets/skills/orbit/references/operational-logs.md"),
    ),
    // Setting Orbit up.
    (
        "orbit/references/setup/first-run.md",
        include_str!("../../assets/skills/orbit/references/setup/first-run.md"),
    ),
    (
        "orbit/references/setup/linux-sandbox.md",
        include_str!("../../assets/skills/orbit/references/setup/linux-sandbox.md"),
    ),
    (
        "orbit/references/setup/configuration.md",
        include_str!("../../assets/skills/orbit/references/setup/configuration.md"),
    ),
    (
        "orbit/references/setup/automation.md",
        include_str!("../../assets/skills/orbit/references/setup/automation.md"),
    ),
    (
        "orbit/references/setup/auto-tasks.md",
        include_str!("../../assets/skills/orbit/references/setup/auto-tasks.md"),
    ),
    (
        "orbit/references/setup/publication.md",
        include_str!("../../assets/skills/orbit/references/setup/publication.md"),
    ),
    (
        "orbit/references/setup/maintenance.md",
        include_str!("../../assets/skills/orbit/references/setup/maintenance.md"),
    ),
    (
        "orbit/references/setup/multi-host.md",
        include_str!("../../assets/skills/orbit/references/setup/multi-host.md"),
    ),
    (
        "orbit/references/setup/remote-access.md",
        include_str!("../../assets/skills/orbit/references/setup/remote-access.md"),
    ),
    // The orchestrator's operating loop, layered on the primitives above.
    (
        "orbit-orchestrate/SKILL.md",
        include_str!("../../assets/skills/orbit-orchestrate/SKILL.md"),
    ),
    (
        "orbit-orchestrate/references/loop.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop.md"),
    ),
    (
        "orbit-orchestrate/references/authorization.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/authorization.md"),
    ),
    (
        "orbit-orchestrate/references/recovery.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery.md"),
    ),
    (
        "orbit-orchestrate/references/walkthroughs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/walkthroughs.md"),
    ),
];

/// The `SKILL.md` entry point of every shipped skill, as `(id, content)`.
/// A skill id is the first path component of its managed asset paths.
pub(crate) fn default_skill_files() -> Vec<(&'static str, &'static str)> {
    DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(relative, content)| {
            relative.strip_suffix("/SKILL.md").map(|id| (id, *content))
        })
        .collect()
}

use crate::paths::ORBIT_ROOT_TOKEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct SkillDoctorResult {
    pub skill_name: String,
    pub status: SkillDoctorStatus,
    pub message: String,
}

pub(crate) fn default_skill_ids() -> Vec<&'static str> {
    default_skill_files()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Materialize the shipped skill trees under `skills_root`, recording the
/// digest Orbit wrote for each file so a skill (or a single reference file)
/// dropped from a later release can be retired by content provenance.
///
/// The digest covers the *rendered* document: `ORBIT_ROOT_TOKEN` resolves to
/// the absolute root before the write, so an unchanged release re-seeds as a
/// no-op on the same root.
// ADR-0366: skills are managed by relative path, so a single reference
// file can be retired independently of its SKILL.md.
pub(crate) fn seed_default_skills(
    skills_root: &Path,
    orbit_root: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        skills_root,
        "skill",
        ManagedAssetLayout::RelativePath,
        &DEFAULT_SKILL_FILES,
        overwrite,
        |_, content| {
            Ok(Cow::Owned(inject_skill_template_tokens(
                content, orbit_root,
            )))
        },
    )
}

pub(crate) fn is_default_skill_file_for_root(
    skill_id: &str,
    path: &Path,
    orbit_root: &Path,
) -> Result<bool, OrbitError> {
    let Some((_, content)) = default_skill_files()
        .into_iter()
        .find(|(default_id, _)| *default_id == skill_id)
    else {
        return Ok(false);
    };
    if !path.exists() {
        return Ok(false);
    }
    let existing = std::fs::read_to_string(path).map_err(|e| OrbitError::Io(e.to_string()))?;
    Ok(existing == inject_skill_template_tokens(content, orbit_root))
}

pub(crate) fn inject_skill_template_tokens(raw: &str, orbit_root: &Path) -> String {
    let orbit_root_value = orbit_root.to_string_lossy();
    raw.replace(ORBIT_ROOT_TOKEN, orbit_root_value.as_ref())
}

impl OrbitRuntime {
    pub fn list_file_skills(&self) -> Result<Vec<LoadedSkill>, OrbitError> {
        self.skill_catalog().list()
    }

    pub fn show_file_skill(&self, name: &str) -> Result<LoadedSkill, OrbitError> {
        self.skill_catalog().load(name)
    }

    pub fn doctor_file_skills(&self) -> Result<Vec<SkillDoctorResult>, OrbitError> {
        let rows = self.skill_catalog().doctor()?;
        let mut results: Vec<SkillDoctorResult> = rows
            .into_iter()
            .map(|row| SkillDoctorResult {
                skill_name: row.skill_id,
                status: match row.status {
                    SkillCatalogDoctorStatus::Ok => SkillDoctorStatus::Ok,
                    SkillCatalogDoctorStatus::Warning => SkillDoctorStatus::Warning,
                    SkillCatalogDoctorStatus::Error => SkillDoctorStatus::Error,
                },
                message: row.message,
            })
            .collect();
        if let Some(home) = crate::paths::home_dir() {
            results.extend(doctor_client_skill_links(
                &crate::bootstrap::init::skill_link_roots(&home),
            )?);
        }
        Ok(results)
    }
}

/// Report dangling/orphaned client skill symlinks under the agent
/// discovery directories (`~/.claude/skills`, `~/.agents/skills`).
///
/// Catalog doctor only walks seeded skill trees. Client CLIs discover
/// skills through these link dirs, so a leftover after a default-set
/// shrink is invisible unless this pass inspects them.
pub(crate) fn doctor_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<SkillDoctorResult>, OrbitError> {
    let mut rows = Vec::new();
    for dir in skills_links_dirs {
        if !dir.exists() {
            continue;
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            paths.push(entry.map_err(|e| OrbitError::Io(e.to_string()))?.path());
        }
        paths.sort();
        for path in paths {
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
            if !meta.file_type().is_symlink() {
                continue;
            }
            if path.exists() {
                continue;
            }
            let skill_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
                .to_string();
            rows.push(SkillDoctorResult {
                skill_name,
                status: SkillDoctorStatus::Error,
                message: format!("dangling skill link at {} (target missing)", path.display()),
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    //! Tests guarding the on-disk assets, seeded registry, and router skill.
    //!
    //! Plus a portability regression (`embedded_assets_are_repository_agnostic`)
    //! guarding the shipped skill *and* activity trees against leaking
    //! Orbit-source paths, private Constellation names, maintainers' personal
    //! names, workspace-local artifact IDs (task/friction/ADR), and
    //! fixed consumer design-doc filenames into public consumer workspaces.

    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn assets_skills_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skills")
    }

    fn assets_activities_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/activities")
    }

    fn collect_relative_files(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = BTreeSet::new();

        while let Some(dir) = pending.pop() {
            let entries =
                std::fs::read_dir(&dir).map_err(|e| format!("read_dir({}): {e}", dir.display()))?;
            for entry in entries {
                let entry = entry.map_err(|e| format!("read_dir({}) entry: {e}", dir.display()))?;
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|e| format!("file_type({}): {e}", path.display()))?;
                if file_type.is_dir() {
                    pending.push(path);
                    continue;
                }
                if file_type.is_file() || file_type.is_symlink() {
                    let relative = path
                        .strip_prefix(root)
                        .map_err(|e| {
                            format!("strip_prefix({}, {}): {e}", root.display(), path.display())
                        })?
                        .to_path_buf();
                    files.insert(relative);
                }
            }
        }

        Ok(files)
    }

    #[test]
    fn asset_dirs_match_default_skill_ids() {
        let dir = assets_skills_dir();
        let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read_dir({}): {e}", dir.display()))
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let file_type = entry.file_type().ok()?;
                if !file_type.is_dir() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('_') {
                    return None;
                }
                Some(name)
            })
            .collect();
        let registered: BTreeSet<String> = default_skill_ids()
            .iter()
            .map(|id| (*id).to_string())
            .collect();

        let missing_from_registry: Vec<&String> = on_disk.difference(&registered).collect();
        let missing_from_disk: Vec<&String> = registered.difference(&on_disk).collect();

        assert!(
            missing_from_registry.is_empty() && missing_from_disk.is_empty(),
            "skill catalogs disagree:\n  in assets/skills/ but NOT in default_skill_ids(): {missing_from_registry:?}\n  in default_skill_ids() but NOT in assets/skills/: {missing_from_disk:?}\nfix by editing crates/orbit-core/src/application/skill.rs::DEFAULT_SKILL_FILES or moving the asset directory under assets/skills/_archive/.",
        );
    }

    /// Each shipped skill's own router must link every one of its own
    /// shipped references.
    ///
    /// Per skill, progressive disclosure runs entirely through that skill's
    /// `SKILL.md` reference table: a reference its own router does not link
    /// is unreachable, however good it is. A skill may still link into
    /// another skill's references (e.g. `orbit-orchestrate` reusing `orbit`'s
    /// canonical docs) — those cross-skill links are not required here since
    /// they are already covered by the target skill's own check.
    #[test]
    fn router_links_every_shipped_reference() {
        let mut missing: Vec<String> = Vec::new();

        for skill_id in default_skill_ids() {
            let router_path = assets_skills_dir().join(skill_id).join("SKILL.md");
            let contents = std::fs::read_to_string(&router_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", router_path.display()));

            let prefix = format!("{skill_id}/");
            for (relative, _) in DEFAULT_SKILL_FILES {
                let Some(reference) = relative.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                if reference == "SKILL.md" {
                    continue;
                }
                // The markdown link target, as written from the skill root.
                if !contents.contains(&format!("({reference})")) {
                    missing.push(relative.to_string());
                }
            }
        }

        assert!(
            missing.is_empty(),
            "a shipped skill's own router does not link these shipped references, so they are \
             unreachable: {missing:?}\nfix by adding a row to that skill's ## References table.",
        );
    }

    // --- Portability regression -------------------------------------------
    //
    // The shipped skill tree is embedded into the binary and seeded into
    // arbitrary consumer workspaces. It must
    // not encode assumptions that only hold in an Orbit source checkout or in
    // Daniel's private Constellation environment. `portability_violations`
    // classifies the leak families the ORB-10208 audit found; the test asserts
    // no shipped file trips them, while allowing genuine public Orbit runtime
    // paths (`.orbit/...`, `~/.orbit/...`) and placeholder IDs (`ORB-NNNN`,
    // `L-NNNN`, `<task-id>`).

    /// Concrete workspace-local artifact IDs (task/friction/decision)
    /// that would become dangling references in a consumer workspace. Placeholder
    /// forms (`ORB-NNNN`, `L-NNNN`, `ADR-NNNN`, `<task-id>`) use non-digit
    /// stand-ins and are intentionally *not* matched.
    fn find_artifact_ids(content: &str, task_prefixes: &BTreeSet<String>) -> Vec<String> {
        let b = content.as_bytes();
        let n = b.len();
        let boundary = |i: usize| i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let digit_run = |start: usize| {
            let mut k = 0;
            while start + k < n && b[start + k].is_ascii_digit() {
                k += 1;
            }
            k
        };
        let starts = |i: usize, pat: &[u8]| i + pat.len() <= n && &b[i..i + pat.len()] == pat;
        let is_digit = |j: usize| j < n && b[j].is_ascii_digit();

        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            if boundary(i) {
                // <registered-prefix>-<digit...> (task ids). Prefixes that only
                // look task-shaped are deliberately ignored: accepting every
                // uppercase token is too weak against ordinary prose.
                for prefix in task_prefixes {
                    let prefix_bytes = prefix.as_bytes();
                    let dash = i + prefix_bytes.len();
                    let digits = dash + 1;
                    if starts(i, prefix_bytes) && dash < n && b[dash] == b'-' && is_digit(digits) {
                        let d = digit_run(digits);
                        let end = digits + d;
                        if end == n || !b[end].is_ascii_alphanumeric() {
                            out.push(String::from_utf8_lossy(&b[i..end]).into_owned());
                        }
                    }
                }
                // ADR-<digit...> (decision-record ids). ADR ids are allocated
                // per workspace, so a concrete one resolves to a different
                // decision — or to nothing — in a consumer's design docs.
                // The ADR-NNNN placeholder has a non-digit after the dash and
                // is skipped.
                if starts(i, b"ADR-") && is_digit(i + 4) {
                    let d = digit_run(i + 4);
                    out.push(String::from_utf8_lossy(&b[i..i + 4 + d]).into_owned());
                }
                // F<yyyy>-<mm>-<nnn> (friction ids). F<YYYY>-<MM>-<NNN> is skipped.
                if b[i] == b'F'
                    && i + 12 <= n
                    && (1..=4).all(|k| is_digit(i + k))
                    && b[i + 5] == b'-'
                    && is_digit(i + 6)
                    && is_digit(i + 7)
                    && b[i + 8] == b'-'
                    && (9..=11).all(|k| is_digit(i + k))
                {
                    out.push(String::from_utf8_lossy(&b[i..i + 12]).into_owned());
                }
                // T<6+ digits> (legacy task ids like T20260514-3).
                if b[i] == b'T' {
                    let d = digit_run(i + 1);
                    if d >= 6 {
                        out.push(String::from_utf8_lossy(&b[i..i + 1 + d]).into_owned());
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// Fixed numbered consumer design-doc filenames such as `4_decisions.md` or
    /// `2_design.md` — an Orbit-source layout convention that does not hold in an
    /// arbitrary consumer repo.
    fn find_numbered_design_doc(content: &str) -> Option<String> {
        let b = content.as_bytes();
        let n = b.len();
        let boundary = |i: usize| i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let mut i = 0;
        while i < n {
            if b[i].is_ascii_digit() && boundary(i) && i + 1 < n && b[i + 1] == b'_' {
                let mut j = i + 2;
                while j < n && (b[j].is_ascii_lowercase() || b[j] == b'_') {
                    j += 1;
                }
                if j > i + 2 && j + 3 <= n && &b[j..j + 3] == b".md" {
                    return Some(String::from_utf8_lossy(&b[i..j + 3]).into_owned());
                }
            }
            i += 1;
        }
        None
    }

    /// Given names of this repository's maintainers. Shipped assets address an
    /// arbitrary consumer's agent, so naming a maintainer turns an enforceable
    /// policy into a referral to someone the reader cannot reach.
    const PRIVATE_PERSONAL_NAMES: &[&str] = &["Daniel"];

    /// Every reason `content` is not repository-agnostic. Empty == portable.
    fn portability_violations_with_task_prefixes(
        content: &str,
        task_prefixes: &BTreeSet<String>,
    ) -> Vec<String> {
        let mut hits = Vec::new();

        // Unguarded Orbit source paths (crate tree only exists in a source clone).
        if content.contains("crates/") {
            hits.push("Orbit source path `crates/...`".to_string());
        }

        // Private / Constellation-specific names.
        for needle in ["almanac", "dk-mac", "dk-server", "Constellation"] {
            if content.contains(needle) {
                hits.push(format!("private name `{needle}`"));
            }
        }

        // Personal names. A shipped asset is read by an agent in someone else's
        // workspace, where a maintainer's given name is an unreachable stranger
        // rather than an authority — so policy must be stated by the *role* that
        // holds it ("the workspace's orchestrator or owner"), never by person.
        for needle in PRIVATE_PERSONAL_NAMES {
            if content.contains(needle) {
                hits.push(format!(
                    "personal name `{needle}` (state the role, not the person)"
                ));
            }
        }

        // A shipped example must not silently select one provider family.
        // Ignore formatting whitespace so both compact and pretty JSON are caught.
        let compact: String = content
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect();
        if compact.contains("\"model\":\"codex\"") {
            hits.push("hard-coded agent model `codex`".to_string());
        }

        // Fixed consumer design-doc filenames.
        if let Some(name) = find_numbered_design_doc(content) {
            hits.push(format!("fixed design-doc filename `{name}`"));
        }

        // Workspace-local artifact IDs.
        for id in find_artifact_ids(content, task_prefixes) {
            hits.push(format!("workspace-local artifact id `{id}`"));
        }

        hits
    }

    fn portability_violations(content: &str) -> Vec<String> {
        portability_violations_with_task_prefixes(content, &BTreeSet::from(["ORB".to_string()]))
    }

    #[test]
    fn task_id_scanner_matches_only_prefixes_known_to_the_local_registry() {
        let root = tempfile::tempdir().expect("registry root");
        let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(root.path()),
        )
        .expect("open task registry");
        registry
            .set_task_prefix("DE")
            .expect("register task prefix");
        let prefixes = registry.known_task_prefixes().expect("known task prefixes");

        assert_eq!(
            find_artifact_ids("known DE-100000 but unknown XY-12345", &prefixes),
            vec!["DE-100000"]
        );
    }

    #[test]
    fn portability_checker_flags_and_allows_representative_inputs() {
        // Fails on representative leaks from each family.
        for bad in [
            "see crates/orbit-core/src/lib.rs",
            "the almanac workspace",
            "hosts: [dk-mac]",
            "part of the Constellation",
            r#"{"model": "codex"}"#,
            "migrated by ORB-00200",
            "the runtime gate (ADR-0250) refuses the call",
            "silently drops it (F2026-05-024)",
            "--evidence task:T20260514-3",
            "docs/design/<feature>/4_decisions.md",
            "put it in 2_design.md",
        ] {
            assert!(
                !portability_violations(bad).is_empty(),
                "portability checker failed to flag a leak: {bad:?}",
            );
        }

        // Allows genuine public Orbit runtime paths and placeholder IDs.
        for good in [
            "decisions live under the feature's reviewed design folder",
            "scheduler state in `~/.orbit/orbit.db` is host-local",
            "evidence under `.orbit/state/job-runs/`",
            "dependencies: [\"ORB-NNNN\", ...] require ORB-NNNNN targets",
            "record the choice as a repo-local ADR-NNNN heading",
            "recommended layout: `docs/design/<feature>/`",
            "resolve `context_files` selectors, then read a `<task-id>`",
            "run `orbit search --kind doc`",
        ] {
            assert!(
                portability_violations(good).is_empty(),
                "portability checker false-positive on portable text {good:?}: {:?}",
                portability_violations(good),
            );
        }
    }

    /// Portability violations and visibly skipped non-text files under `root`.
    #[derive(Debug, PartialEq, Eq)]
    struct PortabilityScan {
        failures: Vec<String>,
        skipped: Vec<String>,
    }

    /// Scan `root`, prefixing each result with its path relative to
    /// `crates/orbit-core/assets/` for a readable failure report.
    fn portability_scan_under(label: &str, root: &Path) -> PortabilityScan {
        let files = collect_relative_files(root)
            .unwrap_or_else(|e| panic!("collect_relative_files({}): {e}", root.display()));

        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        for relative in files {
            let path = root.join(&relative);
            let content = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    skipped.push(format!(
                        "  {label}/{}: skipped non-UTF-8 file",
                        relative.display()
                    ));
                    continue;
                }
                Err(error) => panic!("read {}: {error}", path.display()),
            };
            for violation in portability_violations(&content) {
                failures.push(format!("  {label}/{}: {violation}", relative.display()));
            }
        }
        PortabilityScan { failures, skipped }
    }

    #[test]
    fn portability_checker_skips_non_utf8_files_without_hiding_text_violations() {
        let root = tempfile::tempdir().expect("create temporary assets root");
        std::fs::write(
            root.path().join("bad.md"),
            "See ORB-10530 before handoff.\n",
        )
        .expect("write text fixture");
        std::fs::write(root.path().join("binary.dat"), [0xff, 0xfe]).expect("write binary fixture");

        let scan = portability_scan_under("skills", root.path());
        assert_eq!(
            scan.failures,
            vec!["  skills/bad.md: workspace-local artifact id `ORB-10530`"]
        );
        assert_eq!(
            scan.skipped,
            vec!["  skills/binary.dat: skipped non-UTF-8 file"]
        );
    }

    #[test]
    fn embedded_assets_are_repository_agnostic() {
        // Both trees ship: skills are embedded, and activities are
        // `include_str!`'d by
        // `command::activity` and seeded into every workspace on `orbit init`
        // (with a byte-identical copy under `.orbit/resources/activities/`).
        // Activities were outside this check until a task description's own
        // wording — a maintainer's name plus a concrete ADR id — was copied
        // verbatim into `agent_implement.yaml` (PR #702).
        let skills = portability_scan_under("skills", &assets_skills_dir());
        let activities = portability_scan_under("activities", &assets_activities_dir());
        let mut failures = skills.failures;
        failures.extend(activities.failures);

        assert!(
            failures.is_empty(),
            "embedded assets under crates/orbit-core/assets/{{skills,activities}}/ are not \
             repository-agnostic ({} leak(s)) — generalize, remove, or guard as explicitly \
             source-only/client-specific guidance. State a policy by the role that holds it and \
             the mechanism that enforces it, never by a person's name or a workspace-local \
             artifact id (ORB-10208):\n{}",
            failures.len(),
            failures.join("\n"),
        );
    }

    #[test]
    fn doctor_client_skill_links_reports_dangling_and_ignores_live_targets() {
        let root = tempfile::tempdir().expect("tempdir");
        let claude = root.path().join(".claude").join("skills");
        let agents = root.path().join(".agents").join("skills");
        std::fs::create_dir_all(&claude).expect("create claude skills dir");
        std::fs::create_dir_all(&agents).expect("create agents skills dir");

        let live_target = root.path().join("custom");
        std::fs::create_dir_all(&live_target).expect("create custom skill");
        std::fs::write(live_target.join("SKILL.md"), "# custom\n").expect("write custom skill");
        orbit_common::fs::io::create_dir_symlink(&live_target, &claude.join("my-custom"))
            .expect("create live custom link");

        orbit_common::fs::io::create_dir_symlink(
            &root.path().join("missing-orbit-task"),
            &claude.join("orbit-task"),
        )
        .expect("create dangling claude link");
        orbit_common::fs::io::create_dir_symlink(
            &root.path().join("missing-orbit-search"),
            &agents.join("orbit-search"),
        )
        .expect("create dangling agents link");

        let real_dir = claude.join("operator-dir");
        std::fs::create_dir_all(&real_dir).expect("create operator dir");

        let rows = doctor_client_skill_links(&[claude, agents]).expect("inspect client links");
        assert_eq!(
            rows.len(),
            2,
            "only dangling symlinks are reported: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|row| row.status == SkillDoctorStatus::Error),
            "dangling client links are errors: {rows:?}"
        );
        let names: BTreeSet<&str> = rows.iter().map(|row| row.skill_name.as_str()).collect();
        assert_eq!(
            names,
            BTreeSet::from(["orbit-search", "orbit-task"]),
            "reported names: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|row| row.message.contains("dangling skill link")
                    && row.message.contains("target missing")),
            "messages name the dangling-link condition: {rows:?}"
        );
    }
}
