use std::fs;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::resource::{
    EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource, ExecutorResourceSpec, ResourceKind,
    ResourceMetadata, validate_resource_name,
};
use orbit_types::workflow::ExecutorDef;

use crate::fs::yaml::{parse_yaml_with, write_yaml_atomic_with};

pub struct ExecutorDefFileStore {
    root: PathBuf,
}

impl ExecutorDefFileStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn executors_dir(&self) -> PathBuf {
        self.root.clone()
    }

    fn executor_path(&self, name: &str) -> Result<PathBuf, OrbitError> {
        validate_resource_name(name)?;
        Ok(self.executors_dir().join(format!("{name}.yaml")))
    }

    pub fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError> {
        let dir = self.executors_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut defs = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| OrbitError::Io(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                let content =
                    fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
                let def = parse_executor_def(&content, path.display().to_string())?;
                defs.push(def);
            }
        }
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(defs)
    }

    pub fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError> {
        let path = self.executor_path(name)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        let def = parse_executor_def(&content, path.display().to_string())?;
        Ok(Some(def))
    }

    pub fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError> {
        let path = self.executor_path(&def.name)?;
        let dir = self.executors_dir();
        fs::create_dir_all(&dir)?;
        write_yaml_atomic_with(
            &path,
            &ExecutorResource {
                schema_version: EXECUTOR_RESOURCE_SCHEMA_VERSION,
                kind: ResourceKind::Executor,
                metadata: ResourceMetadata::named(def.name.clone()),
                spec: ExecutorResourceSpec {
                    executor_type: def.executor_type,
                    command: def.command.clone(),
                    args: def.args.clone(),
                    stdout_format: def.stdout_format,
                    model_pair_override: def.model_pair_override.clone(),
                    model_flag: def.model_flag.clone(),
                    timeout_seconds: def.timeout_seconds,
                    auth_probe: def.auth_probe.clone(),
                    env: def.env.clone(),
                    sandbox: def.sandbox,
                    allow_fallback: def.allow_fallback,
                    created_at: def.created_at,
                    updated_at: def.updated_at,
                },
            },
            |e| OrbitError::Execution(format!("failed to serialize executor def: {e}")),
        )
    }
}

fn parse_executor_def(content: &str, label: String) -> Result<ExecutorDef, OrbitError> {
    let doc: ExecutorResource = parse_yaml_with(content, std::path::Path::new(&label), |_, e| {
        OrbitError::InvalidInput(format!("invalid executor def at {}: {e}", label))
    })?;
    if doc.kind != ResourceKind::Executor {
        return Err(OrbitError::InvalidInput(format!(
            "invalid executor def at {}: expected kind Executor, found {}",
            label, doc.kind
        )));
    }
    if doc.schema_version != EXECUTOR_RESOURCE_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "invalid executor def at {}: unsupported schemaVersion {}",
            label, doc.schema_version
        )));
    }
    doc.metadata.validate_name()?;
    Ok(ExecutorDef::from_resource_spec(
        doc.metadata.name,
        doc.spec.clone(),
        doc.spec.created_at,
        doc.spec.updated_at,
    ))
}

#[cfg(test)]
#[cfg(test)]
mod tests;
