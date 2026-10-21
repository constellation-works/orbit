//! Checkout-local candidate definitions overlay the shared workspace catalog
//! for inspection only. Writers and the scheduler retain their local catalog.

use std::collections::BTreeSet;
use std::path::PathBuf;

use orbit_common::OrbitError;

use super::loader::{AutoTaskCollection, AutoTaskLoadError, auto_tasks_dir, collect_auto_tasks};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// The catalog supplying this name. A present but invalid local entry
    /// refuses the lookup rather than exposing a different shared definition.
    pub(crate) fn auto_task_read_root(&self, name: &str) -> Result<Option<PathBuf>, OrbitError> {
        for root in self.auto_task_read_roots() {
            if Self::confined_definition_path(&root, name)?.is_some() {
                return Ok(Some(root));
            }
        }
        Ok(None)
    }

    fn auto_task_read_roots(&self) -> Vec<PathBuf> {
        let local = self.local_root();
        let shared = self.shared_root();
        if local == shared {
            vec![local]
        } else {
            vec![local, shared]
        }
    }

    pub(crate) fn auto_task_missing(&self, name: &str, shared_fallback: bool) -> OrbitError {
        let roots = if shared_fallback {
            self.auto_task_read_roots()
        } else {
            vec![self.local_root()]
        };
        let roots = roots
            .iter()
            .map(|root| format!("'{}'", root.display()))
            .collect::<Vec<_>>()
            .join(", ");
        OrbitError::InvalidInput(format!(
            "no such auto-task '{name}'; searched definition roots: {roots}; run `orbit auto-task list` with the same workspace/root selection"
        ))
    }

    /// Union by file identity, including faulty local entries so a broken or
    /// disabled candidate can never reveal an enabled shared counterpart.
    pub(crate) fn collect_readable_auto_tasks(&self) -> AutoTaskCollection {
        let local = self.local_root();
        let mut collection = collect_auto_tasks(&local);
        if local == self.shared_root() {
            return collection;
        }
        let directory = auto_tasks_dir(&local);
        if std::fs::symlink_metadata(&directory)
            .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        {
            if collection.errors.is_empty() {
                collection.errors.push(AutoTaskLoadError {
                    path: Some(directory),
                    message: "auto_tasks must be a regular directory directly under the workspace Orbit directory".into(),
                });
            }
            return collection;
        }
        // Discovery errors that apply to the entire local catalog fail closed.
        if collection
            .errors
            .iter()
            .any(|error| error.path.as_ref() == Some(&directory))
        {
            return collection;
        }
        let mut shadowed = BTreeSet::new();
        match std::fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            collection.errors.push(AutoTaskLoadError {
                                path: Some(directory.clone()),
                                message: error.to_string(),
                            });
                            return collection;
                        }
                    };
                    let path = entry.path();
                    if path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                        })
                        && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                    {
                        shadowed.insert(stem.to_owned());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                collection.errors.push(AutoTaskLoadError {
                    path: Some(directory),
                    message: error.to_string(),
                });
                return collection;
            }
        }
        let mut shared = collect_auto_tasks(&self.shared_root());
        shared
            .definitions
            .retain(|loaded| !shadowed.contains(&loaded.definition.name));
        shared.errors.retain(|error| {
            !error
                .path
                .as_ref()
                .and_then(|path| path.file_stem())
                .and_then(|stem| stem.to_str())
                .is_some_and(|name| shadowed.contains(name))
        });
        collection.definitions.extend(shared.definitions);
        collection.errors.extend(shared.errors);
        collection
            .definitions
            .sort_by(|a, b| a.definition.name.cmp(&b.definition.name));
        collection
    }
}
