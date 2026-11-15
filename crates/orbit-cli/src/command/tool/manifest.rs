use std::path::{Path, PathBuf};

pub(super) fn resolve_manifest_path(tool_path: &Path, explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }

    manifest_candidates(tool_path)
        .into_iter()
        .find(|candidate| candidate.exists())
}

fn manifest_candidates(tool_path: &Path) -> Vec<PathBuf> {
    vec![
        sidecar_manifest_path_with_extension(tool_path, "yaml"),
        sidecar_manifest_path_with_extension(tool_path, "yml"),
        sidecar_manifest_path_with_extension(tool_path, "json"),
    ]
}

const KNOWN_SCRIPT_EXTENSIONS: &[&str] =
    &["sh", "bash", "zsh", "py", "js", "mjs", "cjs", "ts", "rb"];

pub(super) fn tool_file_stem(tool_path: &Path) -> &str {
    let file_name = tool_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("external-tool");
    if let Some((stem, ext)) = file_name.rsplit_once('.')
        && KNOWN_SCRIPT_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
    {
        return stem;
    }
    file_name
}

fn sidecar_manifest_path_with_extension(tool_path: &Path, extension: &str) -> PathBuf {
    let parent = tool_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = tool_file_stem(tool_path);
    parent.join(format!("{stem}.orbit-tool.{extension}"))
}

pub(super) fn infer_tool_name(path: &Path) -> String {
    tool_file_stem(path).to_string()
}
