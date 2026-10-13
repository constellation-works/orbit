//! The apply step's input, read once into typed records.
//!
//! Every field reads into a lenient [`Field`], so a value of the wrong type is
//! recorded rather than failing the parse. The checks that follow then refuse
//! a malformed input in the same order and wording as before, and a field the
//! step tolerates when malformed still reads as absent. Records that consumers
//! outside this step take as JSON keep their verbatim value alongside.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_engine::DispatchError;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, Error as _};
use serde_json::{Map, Value};

use super::super::apply_admission::PreparedTaskSnapshot;
use super::super::source::SourceSnapshot;

/// One input field, read without failing the parse.
#[derive(Default)]
pub(super) enum Field<T> {
    #[default]
    Absent,
    Present(T),
    /// The value did not read as `T`; carries the reader's error.
    Invalid(String),
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Field<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match T::deserialize(value) {
            Ok(value) => Self::Present(value),
            Err(error) => Self::Invalid(error.to_string()),
        })
    }
}

impl<T> Field<T> {
    pub(super) fn get(&self) -> Option<&T> {
        match self {
            Self::Present(value) => Some(value),
            Self::Absent | Self::Invalid(_) => None,
        }
    }

    fn present(self) -> Option<T> {
        match self {
            Self::Present(value) => Some(value),
            Self::Absent | Self::Invalid(_) => None,
        }
    }

    fn require(self, action: &str, message: &str) -> Result<T, DispatchError> {
        self.present()
            .ok_or_else(|| DispatchError::action_failed(action, message))
    }
}

impl Field<String> {
    /// A trimmed, non-empty string, refused as the shared
    /// `required_string` reader refuses it.
    pub(super) fn non_empty(&self, field: &str, action: &str) -> Result<&str, DispatchError> {
        self.get()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!("`{field}` must be a non-empty string"),
                )
            })
    }
}

/// A list that must hold only strings, read item by item so a non-string item
/// is told apart from a value that is not a list.
pub(super) type Strings = Vec<Field<String>>;

fn strings(
    items: &[Field<String>],
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    items
        .iter()
        .map(|item| {
            item.get().cloned().ok_or_else(|| {
                DispatchError::action_failed(action, format!("`{field}` must contain only strings"))
            })
        })
        .collect()
}

fn not_an_array(field: &str, action: &str) -> DispatchError {
    DispatchError::action_failed(action, format!("`{field}` must be an array"))
}

impl Field<Strings> {
    /// A required string list, refused as the shared `required_string_array`
    /// reader refuses it.
    pub(super) fn strings(&self, field: &str, action: &str) -> Result<Vec<String>, DispatchError> {
        let items = self.get().ok_or_else(|| not_an_array(field, action))?;
        strings(items, field, action)
    }
}

impl Field<Option<Strings>> {
    /// An optional string list: absent or null reads as empty.
    fn strings_or_empty(&self, field: &str, action: &str) -> Result<Vec<String>, DispatchError> {
        match self {
            Self::Absent | Self::Present(None) => Ok(Vec::new()),
            Self::Present(Some(items)) => strings(items, field, action),
            Self::Invalid(_) => Err(not_an_array(field, action)),
        }
    }
}

/// A JSON record read into `T` and kept verbatim. A non-object reads as a
/// record whose every field is absent, as a lookup on it finds nothing.
pub(super) struct Record<T> {
    pub(super) raw: Value,
    pub(super) object: bool,
    pub(super) fields: T,
}

impl<'de, T: DeserializeOwned + Default> Deserialize<'de> for Record<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let object = raw.is_object();
        let fields = if object {
            T::deserialize(&raw).map_err(D::Error::custom)?
        } else {
            T::default()
        };
        Ok(Self {
            raw,
            object,
            fields,
        })
    }
}

/// The `apply_task_pilot_results` step input.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ApplyInput {
    prepared: Field<Record<Prepared>>,
    results: Field<Vec<Record<PartitionResult>>>,
    prior_applied_count: Field<u64>,
    carried_task_outcomes: Field<Vec<Value>>,
    ci_sweep_filing: Option<Record<CiSweepFiling>>,
    promotion_authorized: Field<bool>,
    drain_promotion: Option<Value>,
    run_id: Field<String>,
}

/// The prepare step's output, as apply reads it.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Prepared {
    workspace_path: Field<String>,
    mode: Field<String>,
    partitions: Field<Vec<Record<PreparedPartition>>>,
    tasks: Field<Vec<Record<PreparedTask>>>,
    superseded_by_source: Field<Vec<Value>>,
    superseded_during_preparation: Field<Vec<Value>>,
    excluded: Excluded,
    state_automation: Field<Value>,
    source: Field<Value>,
    source_age: Field<Value>,
    task_ids: Field<Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct PreparedPartition {
    pub(super) partition_index: Field<u64>,
    pub(super) task_ids: Field<Strings>,
}

/// One task as prepare snapshotted it.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct PreparedTask {
    pub(super) task_id: Field<String>,
    context_files_before: Field<Strings>,
    status: Field<TaskStatus>,
    title: Field<String>,
    tags: Field<Strings>,
    complexity: Field<Option<TaskComplexity>>,
    validation_tool_warnings: Field<Option<Strings>>,
    material_fingerprint: Field<String>,
    status_neutral_fingerprint: Field<String>,
    material_components: Field<Option<Map<String, Value>>>,
    history_len: Field<Option<usize>>,
    context_creation_identity: Field<Option<String>>,
}

/// Prepare's exclusions: echoed verbatim in the report, and read for the
/// tasks another live pilot already holds.
#[derive(Default)]
struct Excluded {
    raw: Option<Value>,
    entries: Vec<ExcludedTask>,
}

impl<'de> Deserialize<'de> for Excluded {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let entries = if raw.is_array() {
            Vec::<Record<ExcludedTask>>::deserialize(&raw)
                .map_err(D::Error::custom)?
                .into_iter()
                .map(|entry| entry.fields)
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            raw: Some(raw),
            entries,
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct ExcludedTask {
    pub(super) task_id: Field<String>,
    pub(super) reason: Field<String>,
    pub(super) prepared_by_run_ids: Field<Vec<Value>>,
}

/// One partition's pilot result.
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct PartitionResult {
    pub(super) partition_index: Field<u64>,
    pub(super) task_ids: Field<Strings>,
    /// Each assessment stays JSON for the validators that read it.
    pub(super) tasks: Field<Vec<Record<AssessedTask>>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct AssessedTask {
    pub(super) task_id: Field<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct CiSweepFiling {
    pub(super) task_id: Field<String>,
}

/// The apply input once its envelope is checked.
pub(super) struct Envelope {
    /// The prepared payload verbatim, for the claim, source and receipt
    /// readers that take it as JSON.
    pub(super) prepared: Value,
    pub(super) mode: String,
    pub(super) partitions: Vec<Record<PreparedPartition>>,
    pub(super) tasks: Vec<Record<PreparedTask>>,
    pub(super) results: Vec<Record<PartitionResult>>,
    pub(super) prior_applied_count: u64,
    /// Outcomes settled before this apply: the caller's, then those prepare
    /// set aside.
    pub(super) carried_task_outcomes: Vec<Value>,
    /// Prepare's exclusions, read for the tasks another live pilot holds.
    pub(super) excluded: Vec<ExcludedTask>,
    pub(super) echo: PreparedEcho,
    pub(super) authority: AuthorityInput,
}

/// Prepared fields the report repeats verbatim.
pub(super) struct PreparedEcho {
    pub(super) state_automation: Value,
    pub(super) source: Value,
    pub(super) source_age: Value,
    pub(super) task_ids: Value,
    pub(super) excluded: Value,
}

/// What the caller supplied to authorize promotion.
pub(super) struct AuthorityInput {
    pub(super) ci_sweep_filing: Option<Record<CiSweepFiling>>,
    pub(super) promotion_authorized: Field<bool>,
    pub(super) drain_promotion: Option<Value>,
    pub(super) run_id: Option<String>,
}

impl ApplyInput {
    pub(super) fn parse(action: &str, input: &Value) -> Result<Self, DispatchError> {
        if !input.is_object() {
            return Ok(Self::default());
        }
        Self::deserialize(input).map_err(|error| {
            DispatchError::action_failed(action, format!("invalid apply input: {error}"))
        })
    }

    /// Refuse an input whose prepared envelope is malformed or was prepared
    /// for another workspace.
    pub(super) fn envelope(
        self,
        action: &str,
        workspace_root: &Path,
    ) -> Result<Envelope, DispatchError> {
        let (prepared, fields) = match self.prepared {
            Field::Present(Record {
                raw,
                object: true,
                fields,
            }) => (raw, fields),
            _ => {
                return Err(DispatchError::action_failed(
                    action,
                    "`prepared` must be an object",
                ));
            }
        };
        let prepared_workspace = fields
            .workspace_path
            .require(action, "prepared.workspace_path must be a string")?;
        if Path::new(&prepared_workspace) != workspace_root {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "prepared workspace {prepared_workspace} does not match active workspace {}",
                    workspace_root.display()
                ),
            ));
        }
        let mode = fields
            .mode
            .require(action, "prepared.mode must be a string")?;
        let partitions = fields
            .partitions
            .require(action, "prepared.partitions must be an array")?;
        let tasks = fields
            .tasks
            .require(action, "prepared.tasks must be an array")?;
        let results = self.results.require(action, "`results` must be an array")?;
        let mut carried_task_outcomes = match self.carried_task_outcomes {
            Field::Absent => Vec::new(),
            Field::Present(outcomes) => outcomes,
            Field::Invalid(_) => {
                return Err(DispatchError::action_failed(
                    action,
                    "carried_task_outcomes must be an array",
                ));
            }
        };
        // Tasks prepare already set aside — stale at the source, or changed by
        // another writer between selection and hydration — are carried.
        for set_aside in [
            fields.superseded_by_source,
            fields.superseded_during_preparation,
        ] {
            carried_task_outcomes.extend(set_aside.present().into_iter().flatten());
        }
        let echo = PreparedEcho {
            state_automation: fields.state_automation.present().unwrap_or(Value::Null),
            source: fields.source.present().unwrap_or(Value::Null),
            source_age: fields.source_age.present().unwrap_or(Value::Null),
            task_ids: fields
                .task_ids
                .present()
                .unwrap_or_else(|| Value::Array(Vec::new())),
            excluded: fields
                .excluded
                .raw
                .unwrap_or_else(|| Value::Array(Vec::new())),
        };
        Ok(Envelope {
            prepared,
            mode,
            partitions,
            tasks,
            results,
            prior_applied_count: self.prior_applied_count.present().unwrap_or(0),
            carried_task_outcomes,
            excluded: fields.excluded.entries,
            echo,
            authority: AuthorityInput {
                ci_sweep_filing: self.ci_sweep_filing,
                promotion_authorized: self.promotion_authorized,
                drain_promotion: self.drain_promotion,
                run_id: self.run_id.present(),
            },
        })
    }
}

impl AuthorityInput {
    /// Whether the caller authorized promotion; absent means no.
    pub(super) fn promotion_authorized(&self, action: &str) -> Result<bool, DispatchError> {
        match self.promotion_authorized {
            Field::Absent => Ok(false),
            Field::Present(authorized) => Ok(authorized),
            Field::Invalid(_) => Err(DispatchError::action_failed(
                action,
                "promotion_authorized must be a boolean",
            )),
        }
    }
}

/// Each prepared task's snapshot by id, refused when any snapshot is
/// malformed or two share an id.
pub(super) fn snapshots(
    action: &str,
    tasks: &[Record<PreparedTask>],
    source: Option<&SourceSnapshot>,
) -> Result<BTreeMap<String, PreparedTaskSnapshot>, DispatchError> {
    let source_revision = source.map(|source| source.source_revision.as_str());
    let snapshots = tasks
        .iter()
        .map(|task| task.fields.snapshot(action, source_revision))
        .collect::<Result<BTreeMap<_, _>, DispatchError>>()?;
    if snapshots.len() != tasks.len() {
        return Err(DispatchError::action_failed(
            action,
            "prepared.tasks contains duplicate task snapshots",
        ));
    }
    Ok(snapshots)
}

impl PreparedTask {
    fn snapshot(
        &self,
        action: &str,
        source_revision: Option<&str>,
    ) -> Result<(String, PreparedTaskSnapshot), DispatchError> {
        let refuse = |message: String| DispatchError::action_failed(action, message);
        let task_id = self.task_id.non_empty("task_id", action)?;
        let context_files = self
            .context_files_before
            .strings("context_files_before", action)?;
        let status = match &self.status {
            Field::Present(status) => *status,
            Field::Absent => return Err(refuse("prepared task is missing status".into())),
            Field::Invalid(error) => {
                return Err(refuse(format!("prepared task status is invalid: {error}")));
            }
        };
        let title = self.title.non_empty("title", action)?.to_string();
        let tags = self.tags.strings("tags", action)?;
        let complexity = match &self.complexity {
            Field::Absent => None,
            Field::Present(complexity) => *complexity,
            Field::Invalid(error) => {
                return Err(refuse(format!(
                    "prepared task complexity is invalid: {error}"
                )));
            }
        };
        let validation_tool_warnings = self
            .validation_tool_warnings
            .strings_or_empty(super::super::VALIDATION_TOOL_WARNINGS, action)?;
        let material_components = self.material_components(action)?;
        let history_len = match &self.history_len {
            Field::Absent => None,
            Field::Present(history_len) => *history_len,
            Field::Invalid(error) => return Err(refuse(format!("invalid history_len: {error}"))),
        };
        let context_creation_identity = match &self.context_creation_identity {
            Field::Absent => None,
            Field::Present(identity) => identity.clone(),
            Field::Invalid(_) => {
                return Err(refuse(format!(
                    "prepared task {} must be a string or null",
                    super::super::CONTEXT_CREATION_IDENTITY
                )));
            }
        };
        let snapshot = PreparedTaskSnapshot {
            context_files,
            status,
            complexity,
            title,
            tags,
            validation_tool_warnings,
            material: self
                .material_fingerprint
                .get()
                .map(String::as_str)
                .zip(source_revision)
                .map(|(fingerprint, revision)| (fingerprint.to_string(), revision.to_string())),
            status_neutral_fingerprint: self.status_neutral_fingerprint.get().cloned(),
            material_components,
            history_len,
            context_creation_identity,
        };
        Ok((task_id.to_string(), snapshot))
    }

    /// Freshness-component digests captured with the fingerprint, absent on a
    /// payload prepared before components were recorded. A malformed value
    /// refuses the apply instead of dropping the names.
    fn material_components(
        &self,
        action: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DispatchError> {
        let components = match &self.material_components {
            Field::Absent | Field::Present(None) => return Ok(None),
            Field::Present(Some(components)) => components,
            Field::Invalid(_) => {
                return Err(DispatchError::action_failed(
                    action,
                    "prepared task material_components must be an object of component digests",
                ));
            }
        };
        components
            .iter()
            .map(|(key, digest)| match digest {
                Value::String(digest) => Ok((key.clone(), digest.clone())),
                _ => Err(DispatchError::action_failed(
                    action,
                    format!("prepared task material_components.{key} must be a string digest"),
                )),
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map(Some)
    }
}
