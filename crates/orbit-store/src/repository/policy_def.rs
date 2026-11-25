//! Concrete repository for policy definitions (`<name>.yaml` Policy
//! resources), with a workspace directory optionally layered over a global
//! one.
//!
//! Reached only through [`FileSnapshot::policy_def`] (read-only) and
//! [`FileTx::policy_def`] (read-write inside [`FileStore::write`]). The
//! directory layout is `driver/file/policy_def_store`'s; this repository owns
//! the resource format, its mapping to [`PolicyDef`] and the layered read: a
//! name defined in both directories reads as [`PolicyDef::merged`], and a
//! write goes to the workspace (primary) directory. Seeding the shipped
//! defaults is the calling operation's rule.
//!
//! [`FileStore::write`]: crate::file_store::FileStore::write

use std::fs;
use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_policy_resource;
use orbit_types::policy::PolicyDef;
use orbit_types::resource::{
    POLICY_RESOURCE_SCHEMA_VERSION, PolicyResource, PolicyResourceSpec, ResourceKind,
    ResourceMetadata,
};

use crate::driver::file::policy_def_store::{PolicyDefDir, PolicyDefDirs};
use crate::file_store::{FileSnapshot, FileTx};
use crate::fs::yaml::write_yaml_atomic_with;

/// Concrete policy definition repository scoped to a [`FileSnapshot`]
/// (read-only) or a [`FileTx`] (read-write).
pub struct PolicyDefRepository<'a, Access> {
    dirs: &'a PolicyDefDirs,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> PolicyDefRepository<'a, FileSnapshot<'s, PolicyDefDirs>> {
    pub(crate) fn for_snapshot(snapshot: &'a FileSnapshot<'s, PolicyDefDirs>) -> Self {
        Self {
            dirs: snapshot.layout(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> PolicyDefRepository<'a, FileTx<'tx, PolicyDefDirs>> {
    pub(crate) fn for_tx(tx: &'a FileTx<'tx, PolicyDefDirs>) -> Self {
        Self {
            dirs: tx.layout(),
            _access: PhantomData,
        }
    }

    /// Write `def` to the primary directory, replacing any earlier
    /// definition there.
    pub fn put(&self, def: &PolicyDef) -> Result<(), OrbitError> {
        let dir = &self.dirs.primary;
        def.validate()?;
        let path = dir.path(&def.name)?;
        dir.ensure_layout()?;
        write_yaml_atomic_with(
            &path,
            &PolicyResource {
                schema_version: POLICY_RESOURCE_SCHEMA_VERSION,
                kind: ResourceKind::Policy,
                metadata: ResourceMetadata::named(def.name.clone()),
                spec: PolicyResourceSpec {
                    description: def.description.clone(),
                    deny_read: def.deny_read.clone(),
                    deny_modify: def.deny_modify.clone(),
                    fs_profiles: def.fs_profiles.clone(),
                    created_at: def.created_at,
                    updated_at: def.updated_at,
                },
            },
            |e| OrbitError::InvalidInput(format!("failed to serialize policy {}: {e}", def.name)),
        )
    }
}

impl<Access> PolicyDefRepository<'_, Access> {
    /// Every policy definition, ordered by name, layered as the module
    /// describes.
    pub fn list(&self) -> Result<Vec<PolicyDef>, OrbitError> {
        let Some(global) = &self.dirs.global else {
            return list_dir(&self.dirs.primary);
        };
        let global_defs = list_dir(global)?;
        let mut merged = list_dir(&self.dirs.primary)?;
        for def in global_defs {
            if let Some(existing) = merged
                .iter_mut()
                .find(|candidate| candidate.name == def.name)
            {
                *existing = PolicyDef::merged(&def, existing)?;
            } else {
                merged.push(def);
            }
        }
        merged.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(merged)
    }

    /// The definition named `name`, layered as the module describes.
    pub fn get(&self, name: &str) -> Result<Option<PolicyDef>, OrbitError> {
        let Some(global) = &self.dirs.global else {
            return get_in_dir(&self.dirs.primary, name);
        };
        let workspace = get_in_dir(&self.dirs.primary, name)?;
        let global = get_in_dir(global, name)?;
        match (global, workspace) {
            (Some(global), Some(workspace)) => Ok(Some(PolicyDef::merged(&global, &workspace)?)),
            (None, Some(workspace)) => Ok(Some(workspace)),
            (Some(global), None) => Ok(Some(global)),
            (None, None) => Ok(None),
        }
    }
}

fn list_dir(dir: &PolicyDefDir) -> Result<Vec<PolicyDef>, OrbitError> {
    let names = dir.names()?;
    let mut defs = Vec::with_capacity(names.len());
    for name in names {
        if let Some(def) = get_in_dir(dir, &name)? {
            defs.push(def);
        }
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(defs)
}

fn get_in_dir(dir: &PolicyDefDir, name: &str) -> Result<Option<PolicyDef>, OrbitError> {
    let path = dir.path(name)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
    let def = parse_policy_def(&content, format!("policy {name}"))?;
    Ok(Some(def))
}

fn parse_policy_def(content: &str, label: String) -> Result<PolicyDef, OrbitError> {
    let doc = parse_policy_resource(content, &label)?;
    let def = PolicyDef {
        name: doc.metadata.name,
        description: doc.spec.description,
        deny_read: doc.spec.deny_read,
        deny_modify: doc.spec.deny_modify,
        fs_profiles: doc.spec.fs_profiles,
        created_at: doc.spec.created_at,
        updated_at: doc.spec.updated_at,
    };
    def.validate()?;
    Ok(def)
}
