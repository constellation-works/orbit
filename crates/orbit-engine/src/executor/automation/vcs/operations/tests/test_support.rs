use std::fs;
use std::path::Path;

pub(super) fn read_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .expect("read gh call count")
        .trim()
        .parse()
        .expect("gh call count is numeric")
}

pub(super) fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write gh stub");
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(path, permissions).expect("make gh stub executable");
}

pub(super) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
