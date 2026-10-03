use super::admission::{pull, receipt, request};
use super::*;
#[test]
fn desktop_claimed_task_remains_readable_and_is_not_editable() {
    const CHILD: &str = "ORBIT_TEST_DESKTOP_CLAIM_READ_CHILD";
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let name = format!("{module}::desktop_claimed_task_remains_readable_and_is_not_editable");
    if std::env::var(CHILD).ok().as_deref() != Some(&name) {
        let home = TempDir::new().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let result = command
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, &name)
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "isolated claim fixture failed: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed;"));
        return;
    }
    let temp = TempDir::new().unwrap();
    let fixture = Coordinated::open(temp.path());
    let task = fixture.create_task("Claimed task");
    let admitted = receipt(pull(&fixture, &request("desktop-claim")));
    assert!(admitted.claim.is_some());
    let snapshot = fixture
        .backends
        .task
        .task
        .read_desktop_task(&task.id)
        .unwrap();
    assert_eq!(snapshot.task.id, task.id);
    assert!(!snapshot.revision.is_empty());
    assert!(snapshot.write_disabled_reason.is_some());
    assert!(
        fixture
            .backends
            .task
            .task
            .with_task_write_lock(&task.id, &mut || Ok(()))
            .is_err()
    );
}
