//! Unsafe Git sources are refused through the exported resolver.
use orbit_tools::plugin::{PluginSourceRequest, resolve_plugin_source};

const PIN_LABEL: &str = "the `.orbit/plugins.yaml` pin for 'demo'";

/// A source with no pinned digest, as `orbit plugin add <path>` supplies one.
fn unpinned(source: &str) -> PluginSourceRequest<'_> {
    PluginSourceRequest {
        source,
        expected_digest: None,
        digest_origin: PIN_LABEL,
    }
}

#[cfg(unix)]
fn enter_fake_git_child(test: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_PLUGIN_GIT_CHILD").ok().as_deref() == Some(&exact_test) {
        return true;
    }

    let temp = tempfile::tempdir().expect("fake git directory");
    let args_capture = temp.path().join("args");
    let env_capture = temp.path().join("env");
    let quote =
        |value: &std::path::Path| format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"));
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args}\nenv | sort > {env}\ncheckout=''\nfor arg in \"$@\"; do checkout=$arg; done\nmkdir -p \"$checkout/.git\" \"$checkout/.orbit-plugin\"\nprintf 'schemaVersion: 2\\nkind: Plugin\\n' > \"$checkout/.orbit-plugin/plugin.yaml\"\nprintf 'url=https://user:secret@example.com/demo.git\\n' > \"$checkout/.git/config\"\n",
        args = quote(&args_capture),
        env = quote(&env_capture),
    );
    let git = temp.path().join("git");
    std::fs::write(&git, script).expect("write fake git");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))
        .expect("make fake git executable");

    // The fetch shim serves whatever the child test wrote to the body path,
    // so an HTTPS source can be exercised end to end without a network or a
    // TLS server, and its argv can be asserted the same way Git's is.
    let curl_args_capture = temp.path().join("curl-args");
    let body = temp.path().join("body");
    let curl_script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > {args}\nout=''\nprev=''\nfor arg in \"$@\"; do\n  if [ \"$prev\" = '--output' ]; then out=$arg; fi\n  prev=$arg\ndone\n[ -n \"$out\" ] || exit 2\n[ -f {body} ] || exit 22\nif [ \"$out\" = '-' ]; then cat {body}; else cat {body} > \"$out\"; fi\n",
        args = quote(&curl_args_capture),
        body = quote(&body),
    );
    let curl = temp.path().join("curl");
    std::fs::write(&curl, curl_script).expect("write fake curl");
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))
        .expect("make fake curl executable");

    let mut paths = vec![temp.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_PLUGIN_GIT_CHILD", &exact_test)
        .env("ORBIT_TEST_PLUGIN_GIT_ARGS", &args_capture)
        .env("ORBIT_TEST_PLUGIN_GIT_ENV", &env_capture)
        .env("ORBIT_TEST_PLUGIN_CURL_ARGS", &curl_args_capture)
        .env("ORBIT_TEST_PLUGIN_CURL_BODY", &body)
        .env("PATH", std::env::join_paths(paths).expect("fake git PATH"))
        .output()
        .expect("run isolated fake-git test");
    orbit_common::test_env::assert_child_test_passed(
        &exact_test,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    false
}

#[cfg(unix)]
#[test]
fn unsafe_git_sources_are_refused_before_git_is_spawned() {
    if !enter_fake_git_child("unsafe_git_sources_are_refused_before_git_is_spawned") {
        return;
    }
    let args_capture = std::path::PathBuf::from(
        std::env::var_os("ORBIT_TEST_PLUGIN_GIT_ARGS").expect("args capture path"),
    );
    for source in [
        "git+ext::sh -c 'exit 0' %S",
        "git+file:///tmp/plugin",
        "git+-uplugin",
        "git+https://example.com/demo.git#-upload-pack=payload",
    ] {
        let error = resolve_plugin_source(&unpinned(source))
            .expect_err("unsafe Git source must be refused")
            .to_string();
        assert!(
            error.contains(source),
            "refusal must name the source entry {source:?}: {error}"
        );
    }
    assert!(
        !args_capture.exists(),
        "Git must not be spawned for any refused source"
    );
}
