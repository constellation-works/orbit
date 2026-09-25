use super::*;

/// The record carries authority as well as identity: the spawning caller's
/// effective intersection, which dispatch intersects with the recorded
/// allowlist on every callback [ORB-12801].
#[test]
fn the_session_carries_the_callers_effective_tool_ceiling() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        // Unsorted and repeated on purpose: the recorded ceiling must not
        // depend on how the caller happened to spell its own allowlist.
        &[
            "orbit.task.list".to_string(),
            "orbit.search".to_string(),
            "orbit.task.list".to_string(),
        ],
    )
    .expect("mint");
    assert_eq!(
        session.effective_tools(),
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
    let _env = present_token(session.token());

    let identity = resolve_plugin_callback(root.path(), legacy_on)
        .expect("resolve")
        .expect("identified");
    assert_eq!(
        identity.effective_tools,
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
    assert!(identity.ceiling_admits("orbit.search"));
    assert!(!identity.ceiling_admits("orbit.task.update"));
}

/// Two live sessions of the same plugin, minted for callers with different
/// allowlists. A token resolves to its own record's ceiling, not to whichever
/// session happens to be listed first.
#[test]
fn each_token_resolves_to_its_own_ceiling() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut narrow = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        &["orbit.task.list".to_string()],
    )
    .expect("mint narrow");
    narrow
        .bind_pid(std::process::id())
        .expect("bind this process");
    let mut wide = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        &["orbit.task.list".to_string(), "orbit.search".to_string()],
    )
    .expect("mint wide");
    wide.bind_pid(std::process::id())
        .expect("bind this process");

    let ceiling_for = |session: &PluginCallbackSession| {
        let _env = present_token(session.token());
        resolve_plugin_callback(root.path(), legacy_on)
            .expect("resolve")
            .expect("identified")
            .effective_tools
    };
    assert_eq!(ceiling_for(&narrow), ["orbit.task.list".to_string()]);
    assert_eq!(
        ceiling_for(&wide),
        ["orbit.search".to_string(), "orbit.task.list".to_string()]
    );
}

/// A record that states no ceiling is not a session with an unbounded one. It
/// can only be a leftover from a host that predates the ceiling, so it is
/// refused rather than read as authority nobody granted.
#[test]
fn a_record_without_a_ceiling_is_not_a_session() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let token = "ab".repeat(32);
    std::fs::write(
        dir.join(&token),
        br#"{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":0,"starttime":0}"#,
    )
    .expect("write a pre-ceiling record");
    let _env = present_token(&token);

    let error =
        resolve_plugin_callback(root.path(), legacy_on).expect_err("a record with no ceiling");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("credential is missing or invalid"),
        "{error}"
    );
}

/// The janitor's half of refusing a pre-ceiling record. The leftover here
/// names a *live* process with its real start time, so the only thing that
/// makes it not a session is its schema version: it is never honoured, it is
/// counted stale, and the next backend start sweeps it. Skipping it in the
/// janitor's view instead would strand a mode-0600 file under the session
/// directory that no surface reports and no sweep removes [ORB-12879].
#[test]
fn a_pre_ceiling_record_is_never_honoured_but_is_counted_stale_and_swept() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    let key = orbit_common::process::ancestry::process_start_key(std::process::id())
        .expect("current process start key");
    let leftover = dir.join("ab".repeat(32));
    std::fs::write(
        &leftover,
        format!(
            r#"{{"schema_version":1,"plugin":"demo","version":"1.0.0","manifest_digest":"abc","pid":{},"starttime":{}}}"#,
            key.pid, key.starttime
        ),
    )
    .expect("write a pre-ceiling record");

    {
        // Ancestry would name this very process if the record parsed at all.
        let _env = clear_token();
        assert_eq!(
            resolve_plugin_callback(root.path(), legacy_on).expect("resolve"),
            None,
            "a pre-ceiling record must never identify a caller"
        );
    }
    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        1,
        "a record this host cannot read is stale, not invisible"
    );

    // Minting is what a backend start does: it sweeps before it writes.
    let _session = mint(root.path(), "demo");

    assert!(
        !leftover.exists(),
        "the backend-start sweep must remove a pre-ceiling record"
    );
    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        0,
        "the freshly minted session is not itself stale"
    );
}

/// The sweep must not reap a record it caught mid-write. `mint` leaves the
/// file empty between opening it and the single write that fills it, and
/// unlinking one there would destroy a live session — along with the Landlock
/// grant its child reads the record through.
#[test]
fn a_partially_written_record_is_not_counted_stale() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("state/plugin-callbacks");
    std::fs::create_dir_all(&dir).expect("create callback directory");
    for (name, bytes) in [
        ("just-created", &b""[..]),
        ("half-written", &br#"{"schema_version":2,"plugin":"de"#[..]),
    ] {
        std::fs::write(dir.join(name), bytes).expect("write a partial record");
    }

    assert_eq!(
        stale_plugin_callback_session_count(root.path()).expect("count stale"),
        0,
        "ownership of a partial record cannot be established, so it is left alone"
    );
}

/// The credential the host actually issues: the record open on a descriptor.
/// Nothing in the environment and nothing in the process tree names the
/// session, which is the state a backend descendant reaches after `setsid` and
/// a cleared environment [ORB-12841].
#[cfg(unix)]
#[test]
fn the_inherited_descriptor_identifies_the_plugin_with_no_token_and_no_ancestry() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = PluginCallbackSession::mint(
        root.path(),
        &provenance("demo"),
        &["orbit.task.list".to_string(), "orbit.search".to_string()],
    )
    .expect("mint");
    // Bound to a live process this one is no part of, so ancestry cannot be
    // what answers: pid 1 is never this process, its parent, or its group.
    session.bind_pid(1).expect("bind pid 1");
    let (_credential, _env) = present_descriptor(session.path());

    let identity = resolve_plugin_callback(root.path(), legacy_off)
        .expect("resolve")
        .expect("identified by the descriptor");
    assert_eq!(identity.provenance.name, "demo");
    assert_eq!(
        identity.effective_tools,
        ["orbit.search".to_string(), "orbit.task.list".to_string()],
        "the descriptor carries the caller's ceiling, not just the plugin name"
    );
}

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

/// An ordinary caller may have anything at all on the number. That is not a
/// claim to be a plugin, and it must not refuse the call: what refuses a
/// backend that dropped its credential is the unreadable session directory.
#[cfg(unix)]
#[test]
fn an_unrelated_file_on_the_callback_descriptor_is_an_ordinary_caller() {
    let root = tempfile::tempdir().expect("tempdir");
    let unrelated = root.path().join("notes.txt");
    std::fs::write(&unrelated, b"not a session record").expect("write an unrelated file");
    let (_credential, _env) = present_descriptor(&unrelated);

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_off).expect("resolve"),
        None
    );
}

/// With the deprecation off, a caller holding only the retired credential is
/// refused rather than identified — and rather than admitted as an ordinary
/// caller, which is the escape the descriptor exists to close.
#[test]
fn the_retired_token_is_refused_while_the_deprecation_is_off() {
    let root = tempfile::tempdir().expect("tempdir");
    let session = mint(root.path(), "demo");
    let _env = present_token(session.token());

    let error = resolve_plugin_callback(root.path(), legacy_off).expect_err("a retired credential");
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error.to_string().contains("retired callback credential"),
        "{error}"
    );
}

/// The ancestry half of the same rule, and the reason the flag exists: a live
/// session that names this process identifies it only while the deprecation is
/// on.
#[test]
fn ancestry_identifies_only_while_the_deprecation_is_on() {
    let root = tempfile::tempdir().expect("tempdir");
    let mut session = mint(root.path(), "demo");
    session
        .bind_pid(std::process::id())
        .expect("bind this process");
    let _env = clear_token();

    assert_eq!(
        resolve_plugin_callback(root.path(), legacy_on)
            .expect("resolve")
            .expect("identified")
            .provenance
            .name,
        "demo"
    );
    let error = resolve_plugin_callback(root.path(), legacy_off).expect_err("retired ancestry");
    assert!(
        error.to_string().contains("plugin 'demo' presented"),
        "the refusal still names the backend for the audit row: {error}"
    );
}

/// The host holds its own read handle on every live record while it resolves
/// callbacks of its own. One sitting on the number the resolver inspects would
/// identify `orbit mcp serve` as the plugin it just spawned.
#[cfg(unix)]
#[test]
fn the_hosts_own_handle_never_lands_on_the_callback_number() {
    use super::super::super::callback::PLUGIN_CALLBACK_FD;

    let root = tempfile::tempdir().expect("tempdir");
    let sessions: Vec<_> = (0..4).map(|_| mint(root.path(), "demo")).collect();
    for session in &sessions {
        assert!(
            session.credential_fd() > PLUGIN_CALLBACK_FD,
            "the host's handle is {} and the child reads {PLUGIN_CALLBACK_FD}",
            session.credential_fd()
        );
    }
}
