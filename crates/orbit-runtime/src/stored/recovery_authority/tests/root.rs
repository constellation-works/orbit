use super::super::{RecoveryAuthority, append_recovery_authority_denies};
use super::fixtures::fixture;
use orbit_types::policy::ResolvedFsProfile;
use tempfile::TempDir;

/// The regression this module exists for. A symlink standing in for a component
/// *below* the trusted root used to be followed by `create_dir_all` and then
/// declared clean, because the symlink check ran on an already canonicalized
/// path. Each layout below must be refused with nothing created at the
/// redirection target.
#[cfg(unix)]
#[test]
fn a_symlink_below_the_trusted_root_is_refused_before_anything_is_created() {
    use std::os::unix::fs::symlink;

    for (label, link, target_probe) in [
        ("authority parent", "state", "recovery-authority"),
        ("authority root", "state/recovery-authority", "authority.db"),
    ] {
        let global = TempDir::new().expect("global root");
        let elsewhere = TempDir::new().expect("redirection target");
        let link = global.path().join(link);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).expect("link parent");
        }
        symlink(elsewhere.path(), &link).expect("plant redirection symlink");

        let error =
            RecoveryAuthority::open(global.path()).expect_err("a redirected component must fail");
        assert!(
            error.to_string().contains("symlinked path"),
            "`{label}` must be refused as a symlink: {error}",
        );
        assert!(
            !elsewhere.path().join(target_probe).exists(),
            "`{label}` redirected authority state into `{}`",
            elsewhere.path().display(),
        );
        assert!(
            link.symlink_metadata()
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the planted `{label}` link must be left untouched, not written through",
        );

        // The deny appended to a sandbox profile derives from the same root, so
        // it must refuse the redirected layout too rather than name a path the
        // authority never uses.
        let mut resolved = ResolvedFsProfile {
            name: "implementer".to_string(),
            read: vec!["/**".to_string()],
            modify: Vec::new(),
        };
        let error = append_recovery_authority_denies(global.path(), &mut resolved)
            .expect_err("a redirected root must not yield a deny rule");
        assert!(error.to_string().contains("symlinked path"), "{error}");
        assert!(resolved.modify.is_empty());
    }
}

/// A symlinked database file keeps every directory on the way there looking
/// correct while the certificate is read from, and written to, a file outside
/// the protected root.
#[cfg(unix)]
#[test]
fn a_symlinked_authority_database_is_refused() {
    use std::os::unix::fs::symlink;

    for name in ["authority.db", "authority.db-wal", "authority.db-shm"] {
        let (global, _workspace, _accepted) = fixture();
        let elsewhere = TempDir::new().expect("redirection target");
        let planted = elsewhere.path().join("planted.db");
        std::fs::write(&planted, b"planted").expect("planted file");

        let file = global.path().join("state/recovery-authority").join(name);
        std::fs::remove_file(&file).ok();
        symlink(&planted, &file).expect("plant database symlink");

        let error = RecoveryAuthority::open(global.path())
            .expect_err("a symlinked database file must not be opened");
        assert!(
            error.to_string().contains("symlinked path"),
            "`{name}` must be refused as a symlink: {error}",
        );
        assert_eq!(
            std::fs::read(&planted).expect("planted contents"),
            b"planted",
            "`{name}` let the authority write outside the protected root",
        );
    }
}
