//! Concrete repository for executor definitions (`<name>.yaml` Executor
//! resources).
//!
//! Reached only through [`FileSnapshot::executor_def`] (read-only) and
//! [`FileTx::executor_def`] (read-write inside [`FileStore::write`]). The
//! directory layout is `driver/file/executor_def_store`'s; this repository
//! owns the resource format and its mapping to [`ExecutorDef`]. Seeding and
//! migrating the shipped defaults is the calling operation's rule.
//!
//! [`FileStore::write`]: crate::file_store::FileStore::write

use std::fs;
use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_types::resource::{
    EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource, ExecutorResourceSpec, ResourceKind,
    ResourceMetadata,
};
use orbit_types::workflow::ExecutorDef;

use crate::driver::file::executor_def_store::ExecutorDefDir;
use crate::file_store::{FileSnapshot, FileTx};
use crate::fs::yaml::{parse_yaml_with, write_yaml_atomic_with};

/// Concrete executor definition repository scoped to a [`FileSnapshot`]
/// (read-only) or a [`FileTx`] (read-write).
pub struct ExecutorDefRepository<'a, Access> {
    dir: &'a ExecutorDefDir,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ExecutorDefRepository<'a, FileSnapshot<'s, ExecutorDefDir>> {
    pub(crate) fn for_snapshot(snapshot: &'a FileSnapshot<'s, ExecutorDefDir>) -> Self {
        Self {
            dir: snapshot.layout(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ExecutorDefRepository<'a, FileTx<'tx, ExecutorDefDir>> {
    pub(crate) fn for_tx(tx: &'a FileTx<'tx, ExecutorDefDir>) -> Self {
        Self {
            dir: tx.layout(),
            _access: PhantomData,
        }
    }

    /// Write `def` as its `<name>.yaml`, replacing any earlier definition.
    pub fn put(&self, def: &ExecutorDef) -> Result<(), OrbitError> {
        let path = self.dir.path(&def.name)?;
        self.dir.ensure()?;
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
                    inspection_args: def.inspection_args.clone(),
                    image_input: def.image_input.clone(),
                    stdout_format: def.stdout_format,
                    model_pair_override: def.model_pair_override.clone(),
                    model_flag: def.model_flag.clone(),
                    timeout_seconds: def.timeout_seconds,
                    auth_probe: def.auth_probe.clone(),
                    env: def.env.clone(),
                    sandbox: def.sandbox,
                    allow_fallback: def.allow_fallback,
                    host_os: def.host_os.clone(),
                    created_at: def.created_at,
                    updated_at: def.updated_at,
                },
            },
            |e| OrbitError::Execution(format!("failed to serialize executor def: {e}")),
        )
    }

    /// Remove the definition named `name`. `Ok(false)` when absent.
    pub fn delete(&self, name: &str) -> Result<bool, OrbitError> {
        let path = self.dir.path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(OrbitError::Io(error.to_string())),
        }
    }
}

impl<Access> ExecutorDefRepository<'_, Access> {
    /// Every executor definition, ordered by name.
    pub fn list(&self) -> Result<Vec<ExecutorDef>, OrbitError> {
        let mut defs = Vec::new();
        for path in self.dir.yaml_files()? {
            let content = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
            defs.push(parse_executor_def(&content, path.display().to_string())?);
        }
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(defs)
    }

    /// The definition named `name`, if present.
    pub fn get(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError> {
        let path = self.dir.path(name)?;
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        let def = parse_executor_def(&content, path.display().to_string())?;
        Ok(Some(def))
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
        doc.spec,
        None,
        None,
    ))
}
