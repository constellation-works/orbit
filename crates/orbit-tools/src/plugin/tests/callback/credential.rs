#[cfg(unix)]
use super::super::super::callback::resolve_plugin_callback;
#[cfg(unix)]
use super::{legacy_off, mint, present_descriptor};

/// A backend can put any file it likes on the number, so the record has to
/// prove the host wrote it: its own token must name the very inode the caller
/// holds, and only the host may write that directory.
#[cfg(unix)]
#[test]
fn a_forged_record_on_the_callback_descriptor_is_not_a_credential() {
    let root = tempfile::tempdir().expect("tempdir");
    let real = mint(root.path(), "demo");
    // Byte-for-byte the live record, including its token — but somewhere the
    // plugin could have written it.
    let forged = root.path().join("forged-session");
    std::fs::copy(real.path(), &forged).expect("copy the record");
    let (_credential, _env) = present_descriptor(&forged);

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_off).expect("resolve"),
        None,
        "a record the host did not write must not identify anyone"
    );
}
