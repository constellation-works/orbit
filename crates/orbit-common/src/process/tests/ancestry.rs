use super::super::ancestry::{
    ancestor_start_keys, current_parent_pid, current_process_group, process_start_key,
};

#[test]
fn this_process_has_a_start_key_and_appears_in_its_own_ancestry() {
    let pid = std::process::id();
    let Some(self_key) = process_start_key(pid) else {
        return;
    };
    assert_eq!(self_key.pid, pid);
    let ancestors = ancestor_start_keys();
    assert!(
        ancestors.contains(&self_key),
        "self must lead the ancestry walk: {ancestors:?}"
    );
    assert_eq!(ancestors.first().copied(), Some(self_key));
}

#[cfg(unix)]
#[test]
fn process_group_and_parent_are_queryable_without_proc_of_another_pid() {
    let pid = std::process::id();
    let pgid = current_process_group().expect("getpgrp");
    assert!(pgid >= 1, "pgid={pgid}");
    let ppid = current_parent_pid().expect("getppid");
    assert_ne!(ppid, pid, "parent is distinct from self");
}
