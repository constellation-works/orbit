//! Registers the top-level modules of the `runtime` integration binary.
//!
//! This script writes `tests/runtime/.generated_modules.rs`, which
//! `tests/runtime/main.rs` includes. The file is git-ignored. It must sit beside
//! `main.rs`: rustc resolves a `mod` line in an included file from that file's
//! own directory, and a `#[path]` mount would break the children of a directory
//! module. A new `tests/runtime/<name>.rs`, or a `tests/runtime/<name>/mod.rs`
//! directory, compiles without a hand edit to any tracked file. A top-level file
//! that a sibling mounts with `#[path]` is compiled by that sibling, so it is
//! not registered twice. Subdirectory modules are declared by their parent
//! file, as before.
//!
//! The script never fails the build. A problem is written into the list as
//! `compile_error!`, so only the runtime test target stops. A failed write is a
//! warning, so a read-only source tree still builds the library.

#![allow(
    clippy::print_stdout,
    reason = "Cargo reads rerun-if-changed instructions only from build script stdout"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The generated file's name. `tests/runtime/main.rs` includes this exact name.
const GENERATED: &str = ".generated_modules.rs";

fn main() {
    // A directory watch is recursive: a new file, or a new `#[path]` in any
    // runtime file, reruns this script.
    println!("cargo:rerun-if-changed=tests/runtime");
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/runtime");
    let list = generate(&runtime);
    let target = runtime.join(GENERATED);
    if fs::read_to_string(&target).ok().as_deref() == Some(list.as_str()) {
        return;
    }
    // Write through a temporary name, so the file is replaced whole.
    let temporary = runtime.join(format!("{GENERATED}.tmp"));
    let written = fs::write(&temporary, &list).and_then(|()| fs::rename(&temporary, &target));
    if let Err(error) = written {
        println!("cargo:warning=cannot write {}: {error}", target.display());
    }
}

/// The `mod` list for `tests/runtime/main.rs`, with `compile_error!` for each
/// entry that cannot be registered.
fn generate(runtime: &Path) -> String {
    let mut out = String::from(
        "// Written by crates/orbit-core/build.rs from the files in this directory. Do not edit.\n",
    );
    let mut problems = Vec::new();

    let mut sources = Vec::new();
    collect_rs(runtime, &mut sources);
    let mount_targets = path_mount_targets(&sources);

    let mut names = BTreeSet::new();
    match fs::read_dir(runtime) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else {
                    problems.push(format!(
                        "tests/runtime entry {file_name:?} is not valid UTF-8"
                    ));
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    if path.join("mod.rs").is_file() {
                        names.insert(name.to_owned());
                    }
                } else if let Some(stem) = name.strip_suffix(".rs").filter(|stem| *stem != "main") {
                    names.insert(stem.to_owned());
                }
            }
        }
        Err(error) => problems.push(format!("cannot read tests/runtime: {error}")),
    }

    for name in &names {
        if !is_module_name(name) {
            problems.push(format!(
                "tests/runtime/{name} is not a module name; use lowercase letters, digits and underscores"
            ));
            continue;
        }
        let file = runtime.join(format!("{name}.rs"));
        let mounted_by_sibling =
            fs::canonicalize(&file).is_ok_and(|canonical| mount_targets.contains(&canonical));
        if !mounted_by_sibling {
            out.push_str(&format!("mod {name};\n"));
        }
    }

    for problem in problems {
        out.push_str(&format!("compile_error!({problem:?});\n"));
    }
    out
}

/// Every `.rs` file below `dir`, including the crate root and subdirectories.
fn collect_rs(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Canonical targets of every `#[path = "..."]` in `sources`, resolved from the
/// directory of the file that declares them.
fn path_mount_targets(sources: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut targets = BTreeSet::new();
    for source in sources {
        let (Ok(text), Some(dir)) = (fs::read_to_string(source), source.parent()) else {
            continue;
        };
        for literal in path_attribute_literals(&text) {
            if let Ok(target) = fs::canonicalize(dir.join(literal)) {
                targets.insert(target);
            }
        }
    }
    targets
}

/// The string literal of each `#[path = "..."]` attribute in `text`.
fn path_attribute_literals(text: &str) -> Vec<&str> {
    text.match_indices("#[path")
        .filter_map(|(index, keyword)| {
            let rest = text[index + keyword.len()..].trim_start();
            let rest = rest.strip_prefix('=')?.trim_start().strip_prefix('"')?;
            rest.split_once('"').map(|(literal, _)| literal)
        })
        .collect()
}

fn is_module_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_lowercase())
        && chars.all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit())
}
