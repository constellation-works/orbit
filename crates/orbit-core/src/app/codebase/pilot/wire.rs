//! Tolerant deserializers for the slice of a task-pilot run's pipeline state
//! that attribution reads.

use std::collections::BTreeMap;

use orbit_types::workflow::ActivityCrewDraw;
use serde::Deserialize;

/// The slice of a task-pilot run's pipeline state attribution reads: its
/// frozen crew draws and, per apply step, the operation id and recorded
/// crew of each applied task. A run's state carries every task it assessed
/// in its accumulated pipeline value and per-step outputs, so deserializing
/// the whole checkpoint to read this slice costs a scan over thousands of
/// pilot runs seconds; this projection skips everything else as it parses.
/// A state whose projection does not parse reads as absent, as an
/// unreadable checkpoint did when it was parsed whole.
#[derive(Deserialize)]
pub(super) struct PilotRunStateView {
    #[serde(default)]
    pub(super) activity_crew_draws: BTreeMap<String, ActivityCrewDraw>,
    pipeline: PilotPipeline,
}

impl PilotRunStateView {
    /// The applied tasks the step named `step_id` checkpointed into
    /// `pipeline`, as `PipelineState::pipeline_step_output` reads them.
    pub(super) fn step_tasks(&self, step_id: &str) -> Option<&[PilotTaskRef]> {
        self.pipeline.0.get(step_id)?.tasks.as_deref()
    }
}

/// A slice of the accumulated pipeline value. `PipelineState::pipeline` is
/// deliberately an arbitrary JSON value; indexing a non-object for a step
/// returns no entry but must not discard the run's independent crew draws.
struct PilotPipeline(BTreeMap<String, PilotStepOutputView>);

impl<'de> Deserialize<'de> for PilotPipeline {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PipelineVisitor;
        impl<'de> serde::de::Visitor<'de> for PipelineVisitor {
            type Value = PilotPipeline;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an accumulated pipeline value")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut steps = BTreeMap::new();
                while let Some(key) = access.next_key::<String>()? {
                    steps.insert(key, access.next_value::<PilotStepOutputView>()?);
                }
                Ok(PilotPipeline(steps))
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotPipeline(BTreeMap::new()))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotPipeline(BTreeMap::new()))
            }
        }
        deserializer.deserialize_any(PipelineVisitor)
    }
}

/// One applied task an apply step's output names.
pub(super) struct PilotTaskRef {
    pub(super) operation_id: Option<String>,
    /// The partition crew the applied result recorded; the field
    /// `PILOT_CREW_FIELD` names.
    pub(super) crew: Option<String>,
}

impl<'de> Deserialize<'de> for PilotTaskRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TaskVisitor;
        impl<'de> serde::de::Visitor<'de> for TaskVisitor {
            type Value = PilotTaskRef;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an applied task")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut operation_id = None;
                let mut crew = None;
                while let Some(key) = access.next_key::<String>()? {
                    match key.as_str() {
                        "operation_id" => operation_id = access.next_value::<PilotString>()?.0,
                        "pilot_crew" => crew = access.next_value::<PilotString>()?.0,
                        _ => {
                            access.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(PilotTaskRef { operation_id, crew })
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotTaskRef {
                    operation_id: None,
                    crew: None,
                })
            }
        }
        deserializer.deserialize_any(TaskVisitor)
    }
}

/// `Value::as_str` returns `None` for every other JSON shape. Keep that
/// behavior when projecting string fields out of arbitrary pipeline output.
struct PilotString(Option<String>);

impl<'de> Deserialize<'de> for PilotString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StringVisitor;
        impl<'de> serde::de::Visitor<'de> for StringVisitor {
            type Value = PilotString;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotString(Some(value.to_string())))
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(PilotString(None))
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                while access
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(PilotString(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotString(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotString(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotString(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotString(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotString(None))
            }
        }
        deserializer.deserialize_any(StringVisitor)
    }
}

/// An apply step's checkpointed output: only its applied tasks. Any other
/// shape — a scalar or array pipeline value, a `tasks` that is no array —
/// reads as no tasks, as indexing the whole value did.
struct PilotStepOutputView {
    tasks: Option<Vec<PilotTaskRef>>,
}

impl<'de> Deserialize<'de> for PilotStepOutputView {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct OutputVisitor;
        impl<'de> serde::de::Visitor<'de> for OutputVisitor {
            type Value = PilotStepOutputView;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a pipeline step output")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut tasks = None;
                while let Some(key) = access.next_key::<String>()? {
                    if key == "tasks" {
                        tasks = access.next_value::<PilotTasks>()?.0;
                    } else {
                        access.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(PilotStepOutputView { tasks })
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while access.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotStepOutputView { tasks: None })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotStepOutputView { tasks: None })
            }
        }
        deserializer.deserialize_any(OutputVisitor)
    }
}

/// An apply output's `tasks`: the array's task refs, or nothing for any
/// other shape.
struct PilotTasks(Option<Vec<PilotTaskRef>>);

impl<'de> Deserialize<'de> for PilotTasks {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TasksVisitor;
        impl<'de> serde::de::Visitor<'de> for TasksVisitor {
            type Value = PilotTasks;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("applied tasks")
            }

            fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut tasks = Vec::new();
                while let Some(task) = access.next_element()? {
                    tasks.push(task);
                }
                Ok(PilotTasks(Some(tasks)))
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                while access
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(PilotTasks(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PilotTasks(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(PilotTasks(None))
            }
        }
        deserializer.deserialize_any(TasksVisitor)
    }
}
