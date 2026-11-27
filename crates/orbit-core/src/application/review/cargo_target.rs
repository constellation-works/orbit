//! Conservative integration-target ownership proof from an immutable Git tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use git2::{ObjectType, Repository, Tree};
use toml::Value;

/// Package targets proven from the candidate's workspace manifests and tree.
#[derive(Default)]
pub(super) struct CargoOwners {
    targets: BTreeMap<String, BTreeSet<String>>,
}

impl CargoOwners {
    /// Unreadable or unsupported workspace layouts prove no ownership.
    pub(super) fn at(root: &Path, tree_id: &str) -> Self {
        Self::read(root, tree_id).unwrap_or_default()
    }

    fn read(root: &Path, tree_id: &str) -> Option<Self> {
        let repo = Repository::open(root).ok()?;
        let tree = repo.find_tree(git2::Oid::from_str(tree_id).ok()?).ok()?;
        let workspace = manifest(&repo, &tree, "Cargo.toml")?;
        let members = workspace.get("workspace")?.get("members")?.as_array()?;
        // Explicit paths avoid approximating Cargo glob expansion or exclusions.
        if workspace.get("workspace")?.get("exclude").is_some() {
            return None;
        }
        let mut paths = Vec::new();
        for member in members {
            let path = member.as_str()?;
            if path.is_empty()
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
                || !path
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_'))
            {
                return None;
            }
            paths.push(path.to_string());
        }
        if workspace.get("package").is_some() {
            paths.push(String::new());
        }
        let mut targets = BTreeMap::new();
        for path in paths {
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            let manifest = manifest(&repo, &tree, &format!("{prefix}Cargo.toml"))?;
            let package = manifest.get("package")?;
            let name = package.get("name")?.as_str()?.to_string();
            let auto = match package.get("autotests") {
                None => true,
                Some(value) => value.as_bool()?,
            };
            let mut tests = BTreeSet::new();
            if auto && let Ok(entry) = tree.get_path(Path::new(&format!("{prefix}tests"))) {
                let dir = entry.to_object(&repo).ok()?.peel_to_tree().ok()?;
                for item in &dir {
                    let item_name = item.name().ok()?;
                    match item.kind()? {
                        ObjectType::Blob
                            if item.filemode() == 0o100644 || item.filemode() == 0o100755 =>
                        {
                            if let Some(name) = item_name.strip_suffix(".rs") {
                                tests.insert(name.to_string());
                            }
                        }
                        ObjectType::Tree => {
                            let sub = item.to_object(&repo).ok()?.peel_to_tree().ok()?;
                            if sub.get_name("main.rs").is_some_and(|entry| {
                                entry.kind() == Some(ObjectType::Blob)
                                    && matches!(entry.filemode(), 0o100644 | 0o100755)
                            }) {
                                tests.insert(item_name.to_string());
                            }
                        }
                        _ => return None,
                    }
                }
            }
            if let Some(explicit) = manifest.get("test") {
                for test in explicit.as_array()? {
                    tests.insert(test.get("name")?.as_str()?.to_string());
                }
            }
            if targets.insert(name, tests).is_some() {
                return None;
            }
        }
        Some(Self { targets })
    }

    pub(super) fn unique_owner(&self, packages: &[&str], target: &str, expected: &str) -> bool {
        let mut seen = BTreeSet::new();
        let mut owners = Vec::new();
        for package in packages {
            if !seen.insert(*package) {
                return false;
            }
            let Some(targets) = self.targets.get(*package) else {
                return false;
            };
            if targets.contains(target) {
                owners.push(*package);
            }
        }
        owners.as_slice() == [expected]
    }
}

fn manifest(repo: &Repository, tree: &Tree<'_>, path: &str) -> Option<Value> {
    let entry = tree.get_path(Path::new(path)).ok()?;
    if !matches!(entry.filemode(), 0o100644 | 0o100755) {
        return None;
    }
    let blob = repo.find_blob(entry.id()).ok()?;
    toml::from_str(std::str::from_utf8(blob.content()).ok()?).ok()
}
