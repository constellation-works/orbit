//! The task-owned filing evidence carried from CI intake through pilot admission.
//!
//! Legacy step input is deliberately lenient. Decode its fields once, then
//! validate only when admission reaches each field, preserving refusal order.
//! Raw values are retained solely for verbatim serialization and source echoes.

use orbit_engine::DispatchError;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use serde_json::{Number, Value, json};

/// A fixed filing row. Snapshot details admission does not interpret stay opaque.
#[derive(Clone, PartialEq, Serialize)]
pub(crate) struct CiSweepFilingSource {
    pub(crate) run_id: CiSweepFilingScalar,
    pub(crate) job_id: CiSweepFilingScalar,
    pub(crate) workflow: CiSweepFilingScalar,
    pub(crate) failed_jobs: Value,
    pub(crate) actual_checkout_shas: Value,
}

/// Source identifiers preserve GitHub's numeric and textual spellings.
#[derive(Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum CiSweepFilingScalar {
    Text(String),
    Number(Number),
    Opaque(Value),
}

impl CiSweepFilingScalar {
    fn decode_legacy(value: &Value) -> Self {
        match value {
            Value::String(text) => Self::Text(text.clone()),
            Value::Number(number) => Self::Number(number.clone()),
            value => Self::Opaque(value.clone()),
        }
    }

    pub(crate) fn display(&self) -> String {
        match self {
            Self::Text(text) => text.trim().to_string(),
            Self::Number(number) => number.to_string(),
            Self::Opaque(_) => String::new(),
        }
    }
}

impl CiSweepFilingSource {
    /// Project a legacy run snapshot into its fixed filing row, including nulls.
    pub(crate) fn decode_legacy(run: &Value) -> Self {
        Self {
            run_id: CiSweepFilingScalar::decode_legacy(&run["run_id"]),
            job_id: CiSweepFilingScalar::decode_legacy(&run["job_id"]),
            workflow: CiSweepFilingScalar::decode_legacy(&run["workflow"]),
            failed_jobs: run["failed_jobs"].clone(),
            actual_checkout_shas: run["actual_checkout_shas"].clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum CiSweepFilingRunUrlsError {
    NotArray,
    NonStringItem,
}

/// A filing's identities and source evidence; validation remains admission-owned.
#[derive(Clone, Default)]
pub(crate) struct CiSweepFilingEvidence {
    pub(crate) task_id: String,
    pub(crate) failure_key: String,
    pub(crate) cluster_key: String,
    pub(crate) workflow: String,
    pub(crate) job: String,
    pub(crate) step: String,
    pub(crate) jobs: Vec<String>,
    pub(crate) tested_commit: String,
    pub(crate) sources: Vec<CiSweepFilingSource>,
    /// Copied run identifiers; no admission decision interprets these values.
    pub(crate) run_ids: Vec<Value>,
    pub(crate) run_urls: Vec<String>,
    pub(crate) ref_kinds: CiSweepFilingRefKinds,
    /// Echoed without interpretation, including malformed legacy values.
    pub(crate) head_branches: Value,
    pub(crate) runner_os: Option<Value>,
    pub(crate) os_tags: Option<Vec<String>>,
    pub(crate) failed_covers: Option<Value>,
    pub(crate) run_urls_error: Option<CiSweepFilingRunUrlsError>,
    /// Never used for field decisions.
    pub(crate) legacy_echo: Option<Value>,
}

/// The only ref-kind distinction admission needs, alongside its verbatim echo.
#[derive(Clone, Default)]
pub(crate) struct CiSweepFilingRefKinds {
    echo: Value,
    release: bool,
    integration: bool,
}

impl CiSweepFilingRefKinds {
    fn decode_legacy(echo: Value) -> Self {
        let kinds = echo.as_array();
        Self {
            release: kinds
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("release"))),
            integration: kinds.is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind.as_str() == Some("integration"))
            }),
            echo,
        }
    }
}

impl CiSweepFilingEvidence {
    pub(crate) fn with_ref_kinds(kinds: Vec<String>) -> Self {
        Self {
            ref_kinds: CiSweepFilingRefKinds::decode_legacy(json!(kinds)),
            ..Self::default()
        }
    }

    /// Compatibility decoder for persisted step inputs, never an eager refusal.
    fn decode_legacy(raw: Value) -> Self {
        let text = |key| {
            raw.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let strings = |key| {
            raw.get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let (run_urls, run_urls_error) = match raw.get("run_urls").and_then(Value::as_array) {
            None => (Vec::new(), Some(CiSweepFilingRunUrlsError::NotArray)),
            Some(items) => match items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
            {
                Some(items) => (items, None),
                None => (Vec::new(), Some(CiSweepFilingRunUrlsError::NonStringItem)),
            },
        };
        Self {
            task_id: text("task_id"),
            failure_key: text("failure_key"),
            cluster_key: text("cluster_key"),
            workflow: text("workflow"),
            job: text("job"),
            step: text("step"),
            jobs: strings("jobs"),
            tested_commit: text("tested_commit"),
            sources: raw
                .get("sources")
                .and_then(Value::as_array)
                .map(|sources| {
                    sources
                        .iter()
                        .map(CiSweepFilingSource::decode_legacy)
                        .collect()
                })
                .unwrap_or_default(),
            run_ids: raw
                .get("run_ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            run_urls,
            ref_kinds: CiSweepFilingRefKinds::decode_legacy(
                raw.get("ref_kinds").cloned().unwrap_or_else(|| json!([])),
            ),
            head_branches: raw
                .get("head_branches")
                .cloned()
                .unwrap_or_else(|| json!([])),
            // These are opaque augmentations, not admission inputs. The raw
            // filing preserves their exact presence and shape when echoed.
            runner_os: None,
            os_tags: None,
            failed_covers: None,
            run_urls_error,
            legacy_echo: Some(raw),
        }
    }

    pub(crate) fn release_head_failure(&self) -> bool {
        self.ref_kinds.release && !self.ref_kinds.integration
    }

    pub(crate) fn ref_kinds_echo(&self) -> &Value {
        &self.ref_kinds.echo
    }

    pub(crate) fn required_task_id(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.task_id, "task_id", action)
    }
    pub(crate) fn required_failure_key(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.failure_key, "failure_key", action)
    }
    pub(crate) fn required_tested_commit(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.tested_commit, "tested_commit", action)
    }
    pub(crate) fn required_workflow(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.workflow, "workflow", action)
    }
    pub(crate) fn required_job(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.job, "job", action)
    }
    pub(crate) fn required_step(&self, action: &str) -> Result<&str, DispatchError> {
        required_filing_text(&self.step, "step", action)
    }
    pub(crate) fn required_run_urls(&self, action: &str) -> Result<&[String], DispatchError> {
        match self.run_urls_error {
            None => Ok(&self.run_urls),
            Some(CiSweepFilingRunUrlsError::NotArray) => Err(DispatchError::action_failed(
                action,
                "`run_urls` must be an array",
            )),
            Some(CiSweepFilingRunUrlsError::NonStringItem) => Err(DispatchError::action_failed(
                action,
                "`run_urls` must contain only strings",
            )),
        }
    }
}

fn required_filing_text<'a>(
    text: &'a str,
    field: &str,
    action: &str,
) -> Result<&'a str, DispatchError> {
    let text = text.trim();
    if text.is_empty() {
        Err(DispatchError::action_failed(
            action,
            format!("`{field}` must be a non-empty string"),
        ))
    } else {
        Ok(text)
    }
}

impl<'de> Deserialize<'de> for CiSweepFilingEvidence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::decode_legacy(Value::deserialize(deserializer)?))
    }
}

impl Serialize for CiSweepFilingEvidence {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(raw) = &self.legacy_echo {
            return raw.serialize(serializer);
        }
        // Preserve json!'s insertion order, followed by optional augmentations.
        let mut record = serializer.serialize_struct(
            "CiSweepFilingEvidence",
            13 + usize::from(self.runner_os.is_some())
                + usize::from(self.os_tags.is_some())
                + usize::from(self.failed_covers.is_some()),
        )?;
        record.serialize_field("task_id", &self.task_id)?;
        record.serialize_field("failure_key", &self.failure_key)?;
        record.serialize_field("cluster_key", &self.cluster_key)?;
        record.serialize_field("workflow", &self.workflow)?;
        record.serialize_field("job", &self.job)?;
        record.serialize_field("step", &self.step)?;
        record.serialize_field("jobs", &self.jobs)?;
        record.serialize_field("tested_commit", &self.tested_commit)?;
        record.serialize_field("sources", &self.sources)?;
        record.serialize_field("run_ids", &self.run_ids)?;
        record.serialize_field("run_urls", &self.run_urls)?;
        record.serialize_field("ref_kinds", self.ref_kinds_echo())?;
        record.serialize_field("head_branches", &self.head_branches)?;
        if let Some(value) = &self.runner_os {
            record.serialize_field("runner_os", value)?;
        }
        if let Some(value) = &self.os_tags {
            record.serialize_field("os_tags", value)?;
        }
        if let Some(value) = &self.failed_covers {
            record.serialize_field("failed_covers", value)?;
        }
        record.end()
    }
}
