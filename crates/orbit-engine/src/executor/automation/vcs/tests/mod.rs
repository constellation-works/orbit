#![allow(missing_docs)]

mod base_obsolescence;
mod freshness;
mod git;
mod operations;

/// Isolate PATH in a child test process so real private VCS operations can run
/// a fake provider without changing the environment of concurrent Rust tests.
#[cfg(unix)]
pub(super) fn with_fake_gh(module: &str, test: &str, script: &str) -> bool {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_GH_CHILD").ok().as_deref() == Some(&exact_test) {
        return true;
    }
    let bin = tempfile::tempdir().expect("fake gh directory");
    let gh = bin.path().join("gh");
    fs::write(&gh, script).expect("write fake provider");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("executable provider");
    let mut paths = vec![bin.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_GH_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("provider PATH"))
        .output()
        .expect("isolated provider test");
    assert!(
        output.status.success(),
        "provider test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// Isolate PATH in a child test process so Git-shim fixtures cannot leak into
/// concurrent Rust tests. The shim delegates to the real Git binary except for
/// owned `worktree add` / `rebase` mutations, which hang after the requested
/// side effect so the parent timeout can fire.
#[cfg(unix)]
pub(crate) fn with_fake_git(module: &str, test: &str, extra_env: &[(&str, String)]) -> bool {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let module = module
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module);
    let exact_test = format!("{module}::{test}");
    if std::env::var("ORBIT_TEST_GIT_CHILD").ok().as_deref() == Some(&exact_test) {
        return true;
    }

    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate Git");
    assert!(real_git.status.success(), "git must be on PATH");
    let real_git = String::from_utf8(real_git.stdout)
        .expect("Git path UTF-8")
        .trim()
        .to_string();
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    // Stamp paths are baked into the script: the Git child environment is
    // cleared and would not see ORBIT_TEST_* variables.
    let worktree_once = extra_env
        .iter()
        .find(|(key, _)| *key == "ORBIT_TEST_GIT_WORKTREE_ONCE")
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    let rebase_once = extra_env
        .iter()
        .find(|(key, _)| *key == "ORBIT_TEST_GIT_REBASE_ONCE")
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    let phrase_fail = extra_env
        .iter()
        .find(|(key, _)| *key == "ORBIT_TEST_GIT_PHRASE_FAIL")
        .map(|(_, value)| value.as_str())
        .unwrap_or("");
    let script = format!(
        r#"#!/bin/bash
set -eu
real={real}
worktree_once={worktree_once}
rebase_once={rebase_once}
phrase_fail={phrase_fail}
cmd=""
sub=""
skip=0
seen_cmd=0
for arg in "$@"; do
  if [ "$skip" -eq 1 ]; then
    skip=0
    continue
  fi
  if [ "$seen_cmd" -eq 0 ]; then
    case "$arg" in
      -c) skip=1; continue ;;
      -*) continue ;;
      *) cmd="$arg"; seen_cmd=1; continue ;;
    esac
  elif [ -z "$sub" ]; then
    sub="$arg"
  fi
done

if [ -n "$phrase_fail" ] && [ "$cmd" = "$phrase_fail" ]; then
  if [ "$cmd" = "rebase" ]; then
    case "$sub" in
      --abort|--continue|--skip|--quit) exec "$real" "$@" ;;
    esac
  fi
  if [ "$cmd" = "worktree" ] && [ "$sub" != "add" ]; then
    exec "$real" "$@"
  fi
  echo "process timed out" >&2
  exit 1
fi

if [ "$cmd" = "worktree" ] && [ "$sub" = "add" ]; then
  if [ -n "$worktree_once" ] && [ ! -f "$worktree_once" ]; then
    touch "$worktree_once"
    "$real" "$@"
    wt=""
    for arg in "$@"; do
      if [ -d "$arg" ]; then
        wt="$arg"
      fi
    done
    if [ -n "$wt" ]; then
      gitdir="$("$real" -C "$wt" rev-parse --absolute-git-dir 2>/dev/null || true)"
      if [ -n "$gitdir" ]; then
        rm -f "$gitdir/HEAD" "$gitdir/index"
      fi
    fi
    sleep 30
    exit 0
  fi
fi

if [ "$cmd" = "rebase" ]; then
  case "$sub" in
    --abort|--continue|--skip|--quit) exec "$real" "$@" ;;
  esac
  if [ -n "$rebase_once" ] && [ ! -f "$rebase_once" ]; then
    touch "$rebase_once"
    rebuilt=()
    for arg in "$@"; do
      rebuilt+=("$arg")
      if [ "$arg" = "rebase" ]; then
        rebuilt+=(-x "sleep 30")
      fi
    done
    exec "$real" "${{rebuilt[@]}}"
  fi
fi

exec "$real" "$@"
"#,
        real = quote(&real_git),
        worktree_once = quote(worktree_once),
        rebase_once = quote(rebase_once),
        phrase_fail = quote(phrase_fail)
    );

    let bin = tempfile::tempdir().expect("fake git directory");
    let git = bin.path().join("git");
    fs::write(&git, script).expect("write fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("executable git");
    let mut paths = vec![bin.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", &exact_test, "--nocapture"])
        .env("ORBIT_TEST_GIT_CHILD", &exact_test)
        .env("PATH", std::env::join_paths(paths).expect("git PATH"));
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command.output().expect("isolated git shim test");
    assert!(
        output.status.success(),
        "git shim test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
