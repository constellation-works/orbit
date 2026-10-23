//! The four dimensions of a regression, read off each `regression_from` link,
//! and the breakdowns, cross-tabs and crew and complexity slices the
//! Regressions ranking reports beside its path rates.
//!
//! - **nature** and **escape** come from the link's typed `regression`
//!   payload. A link stored without one is `absent`, distinct from a filer's
//!   explicit `unclassified`; neither is ever guessed.
//! - **layer** is where the fix landed: the regression task's landed paths,
//!   else its declared footprint, mapped through [`LAYER_TABLE`].
//! - **detected_by** is who found it: the regression task's tags through
//!   [`DETECTOR_TABLE`], else its filer.
//!
//! A filter keeps the links whose dimensions all match. It narrows the path
//! ranking and the per-path detail (a regressed task counts when any of its
//! links matches) and the crew and complexity rates.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    RegressionClassification, RegressionEscape, RegressionNature, Task, TaskOrigin,
};
use serde_json::{Value, json};

use super::facts::{Anchor, CodebaseFacts, TaskFacts, actor_kind};
use super::sections::{Params, Tally, rate_fields};

/// A link stored without a `regression` payload.
const ABSENT: &str = "absent";
/// A regression task with neither a landed diff nor a declared path.
const UNKNOWN_LAYER: &str = "unknown";
/// A path no rule of [`LAYER_TABLE`] matches.
const OTHER_LAYER: &str = "other";
/// Layers that accompany a fix rather than locate it: chosen only when the
/// fix touched nothing else.
const SUPPORTING_LAYERS: &[&str] = &["ci_tests", "docs_skills"];

/// How a [`LAYER_TABLE`] rule matches a repository-relative path.
enum PathRule {
    /// The path starts with this text.
    Prefix(&'static str),
    /// A directory component of the path is exactly this name.
    Dir(&'static str),
    /// The path ends with this text.
    Suffix(&'static str),
}

use PathRule::{Dir, Prefix, Suffix};

/// Every path's layer, from the crate and module layout ARCHITECTURE.md
/// describes. The first row with a matching rule wins, so tests and docs are
/// recognised wherever they sit and a narrow module precedes its crate. A
/// path no row matches is `other`. The rows' order is also the order every
/// breakdown lists layers in.
const LAYER_TABLE: &[(&str, &[PathRule])] = &[
    (
        "ci_tests",
        &[
            Prefix(".github/"),
            Prefix("scripts/ci-"),
            Prefix("scripts/test-"),
            Dir("tests"),
            Dir("testdata"),
            Dir("fixtures"),
        ],
    ),
    (
        "docs_skills",
        &[
            Prefix("docs/"),
            Prefix("crates/orbit-core/assets/skills/"),
            Prefix("plugin/"),
            Suffix(".md"),
        ],
    ),
    (
        "dashboard",
        &[Prefix("crates/orbit-web/"), Prefix("desktop/")],
    ),
    (
        "sandbox_policy",
        &[
            Prefix("crates/orbit-policy/"),
            Prefix("crates/orbit-exec/"),
            Prefix("crates/orbit-types/src/policy/"),
            Prefix("crates/orbit-core/src/adapter/engine_host/actions/sandbox/"),
        ],
    ),
    (
        "store_migration",
        &[
            Prefix("crates/orbit-store/"),
            Prefix("crates/orbit-search/"),
            Prefix("crates/orbit-common/src/migration/"),
        ],
    ),
    (
        "vcs_delivery",
        &[
            Prefix("crates/orbit-engine/src/executor/automation/vcs/"),
            Prefix("crates/orbit-engine/src/executor/automation/ci/"),
            Prefix("crates/orbit-engine/src/executor/automation/review/"),
            Prefix("crates/orbit-core/src/application/landing/"),
            Prefix("crates/orbit-core/src/application/review/"),
            Prefix("crates/orbit-automation/src/delivery/"),
            Prefix("crates/orbit-automation/src/review/"),
        ],
    ),
    (
        "config",
        &[
            Prefix("crates/orbit-config/"),
            Prefix("crates/orbit-registry/"),
            Prefix("crates/orbit-core/src/application/config/"),
        ],
    ),
    (
        "cli_mcp",
        &[
            Prefix("crates/orbit-cli/"),
            Prefix("crates/orbit-mcp/"),
            Prefix("crates/orbit-cmd/"),
            Prefix("crates/orbit-tools/"),
            Prefix("crates/orbit-common/src/governance/"),
            Prefix("crates/orbit-core/src/adapter/tool_host/"),
        ],
    ),
    (
        "runtime_lifecycle",
        &[
            Prefix("crates/orbit-core/"),
            Prefix("crates/orbit-engine/"),
            Prefix("crates/orbit-automation/"),
            Prefix("crates/orbit-agent/"),
            Prefix("crates/orbit-types/src/task/"),
            Prefix("crates/orbit-types/src/workflow/"),
        ],
    ),
];

/// Who found a regression, by the regression task's tags. The first row
/// holding a matching tag wins; a pattern ending in `*` matches by prefix.
/// Without one, an operator's filing is `operator` and anything else
/// `other`. The rows' order, then those two, is the breakdown's order.
const DETECTOR_TABLE: &[(&str, &[&str])] = &[
    (
        "delivery_review",
        &["delivery-code-review", "review-reconciliation:*"],
    ),
    (
        "security_review",
        &[
            "security-review",
            "code-scanning-sweep",
            "secret-scanning-sweep",
            "dependabot-sweep",
        ],
    ),
    ("code_review", &["code-review", "full-code-review"]),
    ("qa_sweep", &["qa-sweep*", "qa-full-sweep*"]),
    (
        "post_merge_ci",
        &[
            "ci-failure-sweep",
            "ci-failure*",
            "ci-red",
            "ci-repair",
            "baseline-red-repair",
            "post-merge-repair",
        ],
    ),
    ("production_run", &["run-failure-patterns"]),
];
const OPERATOR_DETECTOR: &str = "operator";
const OTHER_DETECTOR: &str = "other";

const NATURES: &[&str] = &[
    "broke_existing",
    "defect_in_new",
    "incomplete",
    "merge_reintroduced",
    "contract_drift",
    "unclassified",
    ABSENT,
];
const ESCAPES: &[&str] = &[
    "no_test",
    "test_not_selected",
    "platform_only",
    "review_missed",
    "spec_ambiguous",
    "nondeterministic",
    "unclassified",
    ABSENT,
];

/// The four dimensions, in the order a link, a filter and the breakdowns
/// carry them.
const DIMENSIONS: [&str; 4] = ["nature", "escape", "layer", "detected_by"];
const NATURE: usize = 0;
const ESCAPE: usize = 1;
const LAYER: usize = 2;

/// Every value a dimension takes, in breakdown order.
fn values(dimension: usize) -> Vec<&'static str> {
    match dimension {
        NATURE => NATURES.to_vec(),
        ESCAPE => ESCAPES.to_vec(),
        LAYER => LAYER_TABLE
            .iter()
            .map(|(layer, _)| *layer)
            .chain([OTHER_LAYER, UNKNOWN_LAYER])
            .collect(),
        _ => DETECTOR_TABLE
            .iter()
            .map(|(detector, _)| *detector)
            .chain([OPERATOR_DETECTOR, OTHER_DETECTOR])
            .collect(),
    }
}

/// Crews and complexities ranked beside the paths: `crew` (else the
/// implementer) and `complexity` of the regressed task.
const SLICES: [&str; 2] = ["crew", "complexity"];

/// One `regression_from` link, read from the regressed task's side.
#[derive(Debug, Clone)]
pub(super) struct RegressionLink {
    /// The regression task: it carries the link and fixes the defect.
    pub(super) task_id: String,
    /// nature, escape, layer and detected_by, as [`DIMENSIONS`] orders them.
    dims: [&'static str; 4],
    /// `landed`, `footprint` or `none`: what the layer was read from.
    layer_source: &'static str,
    /// When the regression task was filed.
    pub(super) filed_at: DateTime<Utc>,
    /// The regression task's title: its one-line account of the defect.
    pub(super) summary: String,
}

impl RegressionLink {
    /// The link's nature; a link stored without a payload reads as
    /// `unclassified`, as its filer never said.
    fn nature_or_unclassified(&self) -> &'static str {
        classified(self.dims[NATURE])
    }

    /// The link's escape, with an absent payload read as `unclassified`.
    fn escape_or_unclassified(&self) -> &'static str {
        classified(self.dims[ESCAPE])
    }

    pub(super) fn json(&self) -> Value {
        json!({
            "task_id": self.task_id,
            "nature": self.dims[0],
            "escape": self.dims[1],
            "layer": self.dims[2],
            "layer_source": self.layer_source,
            "detected_by": self.dims[3],
        })
    }
}

/// A link as the scan first reads it, before the regression task's paths
/// are known.
pub(super) struct PendingLink {
    pub(super) regressed: String,
    pub(super) task_id: String,
    nature: &'static str,
    escape: &'static str,
    detected_by: &'static str,
    filed_at: DateTime<Utc>,
    summary: String,
}

impl PendingLink {
    /// The link `fix`, the regression task, records against `regressed`.
    pub(super) fn new(
        regressed: String,
        fix: &Task,
        classification: Option<RegressionClassification>,
    ) -> Self {
        let (nature, escape) = classification.map_or((ABSENT, ABSENT), |classification| {
            (
                nature_name(classification.nature),
                escape_name(classification.escape),
            )
        });
        Self {
            regressed,
            task_id: fix.id.to_string(),
            nature,
            escape,
            detected_by: detected_by(fix.created_by.as_deref(), fix.origin, &fix.tags),
            filed_at: fix.created_at,
            summary: fix.title.clone(),
        }
    }

    /// Complete the link with the layer of `fix`, the regression task.
    pub(super) fn resolve(self, fix: Option<&TaskFacts>) -> RegressionLink {
        let (layer, layer_source) = fix.map_or((UNKNOWN_LAYER, "none"), fix_layer);
        RegressionLink {
            task_id: self.task_id,
            dims: [self.nature, self.escape, layer, self.detected_by],
            layer_source,
            filed_at: self.filed_at,
            summary: self.summary,
        }
    }
}

/// A nature or escape as a regression-risk mark shows it: an absent payload
/// is as unclassified as an explicit `unclassified`.
fn classified(value: &'static str) -> &'static str {
    if value == ABSENT {
        "unclassified"
    } else {
        value
    }
}

/// The `limit` natures and escapes most of `links` carry, each by link
/// count, ties in breakdown order. An unclassified link shows as
/// `unclassified`; nothing is guessed.
pub(super) fn top_categories(
    links: &[&RegressionLink],
    limit: usize,
) -> (Vec<&'static str>, Vec<&'static str>) {
    let top = |order: &[&'static str], value: fn(&RegressionLink) -> &'static str| {
        let mut counted: Vec<(&'static str, usize)> = order
            .iter()
            .filter(|candidate| **candidate != ABSENT)
            .map(|candidate| {
                let count = links
                    .iter()
                    .filter(|link| value(link) == *candidate)
                    .count();
                (*candidate, count)
            })
            .filter(|(_, count)| *count > 0)
            .collect();
        // A stable sort keeps breakdown order among equal counts.
        counted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        counted
            .into_iter()
            .take(limit)
            .map(|(candidate, _)| candidate)
            .collect()
    };
    (
        top(NATURES, RegressionLink::nature_or_unclassified),
        top(ESCAPES, RegressionLink::escape_or_unclassified),
    )
}

fn nature_name(nature: RegressionNature) -> &'static str {
    match nature {
        RegressionNature::BrokeExisting => "broke_existing",
        RegressionNature::DefectInNew => "defect_in_new",
        RegressionNature::Incomplete => "incomplete",
        RegressionNature::MergeReintroduced => "merge_reintroduced",
        RegressionNature::ContractDrift => "contract_drift",
        RegressionNature::Unclassified => "unclassified",
    }
}

fn escape_name(escape: RegressionEscape) -> &'static str {
    match escape {
        RegressionEscape::NoTest => "no_test",
        RegressionEscape::TestNotSelected => "test_not_selected",
        RegressionEscape::PlatformOnly => "platform_only",
        RegressionEscape::ReviewMissed => "review_missed",
        RegressionEscape::SpecAmbiguous => "spec_ambiguous",
        RegressionEscape::Nondeterministic => "nondeterministic",
        RegressionEscape::Unclassified => "unclassified",
    }
}

fn detected_by(
    created_by: Option<&str>,
    origin: Option<TaskOrigin>,
    tags: &[String],
) -> &'static str {
    let matches = |pattern: &str, tag: &str| match pattern.strip_suffix('*') {
        Some(prefix) => tag.starts_with(prefix),
        None => tag == pattern,
    };
    for (detector, patterns) in DETECTOR_TABLE {
        let tagged = tags
            .iter()
            .any(|tag| patterns.iter().any(|pattern| matches(pattern, tag.trim())));
        if tagged {
            return detector;
        }
    }
    let by_operator = origin == Some(TaskOrigin::OperatorRequest)
        || created_by.is_some_and(|by| actor_kind(by) == "operator");
    if by_operator {
        OPERATOR_DETECTOR
    } else {
        OTHER_DETECTOR
    }
}

/// The layer of one path: the first [`LAYER_TABLE`] row with a matching rule.
fn path_layer(path: &str) -> &'static str {
    let matched = |rule: &PathRule| match rule {
        Prefix(prefix) => path.starts_with(prefix),
        Suffix(suffix) => path.ends_with(suffix),
        // Every component but the last names a directory; a directory
        // anchor's path ends in `/`, so its own name counts too.
        Dir(name) => {
            let mut components: Vec<&str> = path.split('/').collect();
            components.pop();
            components.contains(name)
        }
    };
    LAYER_TABLE
        .iter()
        .find(|(_, rules)| rules.iter().any(matched))
        .map_or(OTHER_LAYER, |(layer, _)| layer)
}

/// The layer a fix sits in, and what it was read from: the layer most of its
/// landed paths (else its declared footprint) map to, a supporting layer
/// only when nothing else was touched; ties go to the earlier layer.
fn fix_layer(fix: &TaskFacts) -> (&'static str, &'static str) {
    let landed: Vec<String> = fix
        .landed
        .as_ref()
        .map(|landed| landed.paths.iter().cloned().collect())
        .unwrap_or_default();
    let (paths, source) = if landed.is_empty() {
        let declared: Vec<String> = fix.footprint.iter().map(anchor_path).collect();
        (declared, "footprint")
    } else {
        (landed, "landed")
    };
    if paths.is_empty() {
        return (UNKNOWN_LAYER, "none");
    }
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for path in &paths {
        *counts.entry(path_layer(path)).or_default() += 1;
    }
    let order = values(LAYER);
    let pick = |supporting: bool| {
        order
            .iter()
            .filter(|layer| SUPPORTING_LAYERS.contains(layer) == supporting)
            .filter_map(|layer| Some((*layer, *counts.get(layer)?)))
            // `max_by_key` keeps the last maximum; reversing keeps the first.
            .rev()
            .max_by_key(|(_, count)| *count)
            .map(|(layer, _)| layer)
    };
    let layer = pick(false).or_else(|| pick(true)).unwrap_or(UNKNOWN_LAYER);
    (layer, source)
}

fn anchor_path(anchor: &Anchor) -> String {
    if anchor.dir {
        format!("{}/", anchor.path)
    } else {
        anchor.path.clone()
    }
}

/// A selection of regressions: at most one value per dimension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegressionFilter {
    dims: [Option<&'static str>; 4],
}

impl RegressionFilter {
    /// Validate a filter. Each value must be one its dimension takes,
    /// `absent` and `unclassified` included for nature and escape, and
    /// `unknown` for a layer.
    pub fn parse(
        nature: Option<&str>,
        escape: Option<&str>,
        layer: Option<&str>,
        detected_by: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let mut dims = [None; 4];
        for (index, raw) in [nature, escape, layer, detected_by].into_iter().enumerate() {
            let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
                continue;
            };
            let known = values(index);
            let Some(value) = known.iter().find(|value| **value == raw) else {
                return Err(OrbitError::InvalidInput(format!(
                    "{} must be one of {}; got '{raw}'",
                    DIMENSIONS[index],
                    known.join(", ")
                )));
            };
            dims[index] = Some(*value);
        }
        Ok(Self { dims })
    }

    /// Whether `link` matches every dimension except `skip`.
    fn admits(&self, link: &RegressionLink, skip: Option<usize>) -> bool {
        self.dims
            .iter()
            .zip(link.dims)
            .enumerate()
            .all(|(index, (wanted, value))| {
                Some(index) == skip || wanted.is_none_or(|w| w == value)
            })
    }

    /// The links of `task` the filter keeps.
    pub(super) fn links<'a>(&self, task: &'a TaskFacts) -> Vec<&'a RegressionLink> {
        task.regressions
            .iter()
            .filter(|link| self.admits(link, None))
            .collect()
    }

    /// Whether any link of `task` matches.
    pub(super) fn regressed(&self, task: &TaskFacts) -> bool {
        task.regressions.iter().any(|link| self.admits(link, None))
    }

    pub(super) fn json(&self) -> Value {
        Value::Object(
            DIMENSIONS
                .iter()
                .zip(self.dims)
                .map(|(dimension, value)| (dimension.to_string(), json!(value)))
                .collect(),
        )
    }
}

/// The breakdowns, cross-tabs and crew and complexity rates for the done
/// tasks of the window, merged into the `regressions` section.
///
/// Breakdowns count links per value of one dimension under the filter's
/// other dimensions, so a chip shows what choosing it would leave. The
/// cross-tabs count links under the whole filter. A crew or complexity row
/// is its done tasks with a matching link ÷ its done tasks, ranked when it
/// has the minimum support.
pub(super) fn summary(facts: &CodebaseFacts, params: &Params) -> Value {
    let filter = &params.filter;
    let done: Vec<&TaskFacts> = facts
        .tasks
        .iter()
        .filter(|task| task.done_at.is_some_and(|at| params.within(at)))
        .collect();
    let links: Vec<&RegressionLink> = done
        .iter()
        .flat_map(|task| task.regressions.iter())
        .collect();

    let breakdowns: serde_json::Map<String, Value> = DIMENSIONS
        .iter()
        .enumerate()
        .map(|(index, dimension)| {
            let rows: Vec<Value> = values(index)
                .into_iter()
                .map(|value| {
                    let count = links
                        .iter()
                        .filter(|link| {
                            link.dims[index] == value && filter.admits(link, Some(index))
                        })
                        .count();
                    json!({ "value": value, "links": count })
                })
                .collect();
            (dimension.to_string(), Value::Array(rows))
        })
        .collect();

    let kept: Vec<&RegressionLink> = links
        .iter()
        .copied()
        .filter(|link| filter.admits(link, None))
        .collect();
    let mut slices = serde_json::Map::new();
    for slice in SLICES {
        let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
        for task in &done {
            let key = match slice {
                "crew" => task.crew.clone().unwrap_or_else(|| "unassigned".into()),
                _ => task.complexity.clone().unwrap_or_else(|| "unset".into()),
            };
            let tally = tallies.entry(key).or_default();
            tally.denominator += 1;
            if filter.regressed(task) {
                tally.numerator += 1;
            }
        }
        let below_support = tallies
            .values()
            .filter(|tally| tally.denominator < params.min_support)
            .count();
        let mut rows: Vec<(String, Tally)> = tallies
            .into_iter()
            .filter(|(_, tally)| tally.denominator >= params.min_support)
            .collect();
        rows.sort_by(|(left_key, left), (right_key, right)| {
            let rate = |tally: &Tally| tally.numerator as f64 / tally.denominator as f64;
            rate(right)
                .total_cmp(&rate(left))
                .then_with(|| right.denominator.cmp(&left.denominator))
                .then_with(|| left_key.cmp(right_key))
        });
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|(key, tally)| {
                let mut row = rate_fields(tally.numerator, tally.denominator);
                row[slice] = json!(key);
                row["support"] = json!(tally.denominator);
                row
            })
            .collect();
        slices.insert(
            format!("by_{slice}"),
            json!({ "rows": rows, "below_support": below_support }),
        );
    }

    let mut summary = json!({
        "filter": filter.json(),
        "links": links.len(),
        "matching_links": kept.len(),
        "regressed": done.iter().filter(|task| filter.regressed(task)).count(),
        "breakdowns": breakdowns,
        "cross_tabs": {
            "nature_escape": cross_tab(&kept, NATURE, ESCAPE),
            "escape_layer": cross_tab(&kept, ESCAPE, LAYER),
        },
    });
    if let Some(object) = summary.as_object_mut() {
        object.extend(slices);
    }
    summary
}

/// Link counts by one dimension (rows) and another (columns), each listing
/// only the values some link takes, in breakdown order.
fn cross_tab(links: &[&RegressionLink], row: usize, column: usize) -> Value {
    let present = |dimension: usize| -> Vec<&'static str> {
        values(dimension)
            .into_iter()
            .filter(|value| links.iter().any(|link| link.dims[dimension] == *value))
            .collect()
    };
    let rows = present(row);
    let columns = present(column);
    let counts: Vec<Vec<usize>> = rows
        .iter()
        .map(|row_value| {
            columns
                .iter()
                .map(|column_value| {
                    links
                        .iter()
                        .filter(|link| {
                            link.dims[row] == *row_value && link.dims[column] == *column_value
                        })
                        .count()
                })
                .collect()
        })
        .collect();
    json!({
        "row": DIMENSIONS[row],
        "column": DIMENSIONS[column],
        "rows": rows,
        "columns": columns,
        "counts": counts,
    })
}
