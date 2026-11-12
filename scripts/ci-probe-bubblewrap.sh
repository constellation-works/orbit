#!/usr/bin/env bash
# Prepare the hosted Linux boundary-test environment; used from the workflow revision.
set -euo pipefail
started=$SECONDS
step_budget=480
package_seconds=0
package_started=""
probe_seconds=0
profile_src=/usr/share/apparmor/extra-profiles/bwrap-userns-restrict
profile_dst=/etc/apparmor.d/bwrap-userns-restrict
probe_err="$(mktemp)"
phase_log="$(mktemp)"
report_setup() {
  if [ -n "$package_started" ]; then
    package_seconds=$((SECONDS - package_started))
  fi
  echo "bubblewrap_setup_seconds=$((SECONDS - started)) package_seconds=$package_seconds probe_seconds=$probe_seconds"
  rm -f "$probe_err" "$phase_log"
}
trap report_setup EXIT

say() {
  echo "[bubblewrap +$((SECONDS - started))s] $*"
}

# Each diagnostic is bounded too: a hung one must not mask the error.
fail() {
  echo "::error title=Bubblewrap unavailable::$1"
  echo "--- diagnostics ---"
  echo "user: $(id)"
  echo "kernel.apparmor_restrict_unprivileged_userns=$(timeout 5 sysctl -n kernel.apparmor_restrict_unprivileged_userns 2>&1 || true)"
  echo "kernel.unprivileged_userns_clone=$(timeout 5 sysctl -n kernel.unprivileged_userns_clone 2>&1 || true)"
  echo "user.max_user_namespaces=$(timeout 5 sysctl -n user.max_user_namespaces 2>&1 || true)"
  timeout 5 ls -l /usr/bin/bwrap 2>&1 || true
  timeout 5 /usr/bin/bwrap --version 2>&1 || true
  echo "bwrap probe stderr: $(cat "$probe_err" 2>/dev/null || true)"
  sudo timeout 5 grep -E '^bwrap ' /sys/kernel/security/apparmor/profiles 2>&1 || echo "no bwrap AppArmor profile loaded (or the profile list is unreadable)"
  exit 1
}

timed_out() {
  [ "$1" -eq 124 ] || [ "$1" -eq 137 ]
}

# phase <name> <limit seconds> [--root] <command...>: stream the
# command's output, bound it with timeout (inside sudo for --root, so
# the command itself is signalled), and leave its status in the return
# value and its output in $phase_log. The limit shrinks to what is
# left of the step budget.
phase() {
  local name=$1 limit=$2 phase_started=$SECONDS rc=0
  local -a prefix=(timeout --kill-after=5)
  shift 2
  if [ "$1" = "--root" ]; then
    shift
    prefix=(sudo timeout --kill-after=5)
  fi
  local remaining=$((step_budget - (SECONDS - started)))
  [ "$remaining" -gt 0 ] || fail "$name not started: the ${step_budget}s step budget is spent"
  [ "$limit" -le "$remaining" ] || limit=$remaining
  say "$name: start (limit ${limit}s)"
  set +o pipefail
  "${prefix[@]}" "$limit" "$@" 2>&1 | tee "$phase_log"
  rc=${PIPESTATUS[0]}
  set -o pipefail
  say "$name: end after $((SECONDS - phase_started))s (exit $rc)"
  PHASE_LIMIT=$limit
  return "$rc"
}

# A phase that must succeed on its first run.
must_phase() {
  local rc=0
  phase "$@" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  if timed_out "$rc"; then
    fail "$1 timed out after ${PHASE_LIMIT}s"
  fi
  fail "$1 failed (exit $rc): $(tail -n 5 "$phase_log" | tr '\n' ' ')"
}

# An apt lock or fetch failure, or a timeout of either, is transient.
apt_transient() {
  timed_out "$1" || grep -Eiq \
    'Could not get lock|Unable to acquire the dpkg|Unable to lock|Failed to fetch|Some index files failed|Temporary failure resolving|Could not (resolve|connect)|Connection (timed out|failed|reset)|Hash Sum mismatch' \
    "$phase_log"
}

# A killed apt-get can leave dpkg interrupted, and apt then refuses
# every install until `dpkg --configure -a` finishes the unpack. The
# killed dpkg may still hold the frontend lock for a moment, so the
# recovery retries briefly. Best effort and bounded: the retry judges.
recover_dpkg() {
  local deadline=$((SECONDS + 60)) rc=0
  while :; do
    rc=0
    phase "dpkg --configure -a" 60 --root env DEBIAN_FRONTEND=noninteractive \
      NEEDRESTART_MODE=l dpkg --configure -a || rc=$?
    [ "$rc" -eq 0 ] && return 0
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 2
  done
}

# An apt phase gets one retry, and only for a transient failure.
apt_phase() {
  local name=$1 limit=$2 attempt rc=0
  shift 2
  for attempt in 1 2; do
    rc=0
    phase "$name (attempt $attempt)" "$limit" --root "$@" || rc=$?
    [ "$rc" -eq 0 ] && return 0
    if [ "$attempt" -eq 2 ] || ! apt_transient "$rc"; then
      break
    fi
    say "$name: transient failure (exit $rc); retrying once"
    recover_dpkg || say "dpkg --configure -a did not finish; the retry decides"
  done
  if timed_out "$rc"; then
    fail "$name timed out after ${PHASE_LIMIT}s"
  fi
  fail "$name failed (exit $rc): $(tail -n 5 "$phase_log" | tr '\n' ' ')"
}

bwrap_profile_loaded() {
  local rc=0
  sudo timeout --kill-after=2 5 grep -Eq '^bwrap ' /sys/kernel/security/apparmor/profiles || rc=$?
  if timed_out "$rc"; then
    fail "reading the AppArmor profile list timed out after 5s"
  fi
  return "$rc"
}

# The same checks Orbit's capability probe makes: the trusted path,
# the --bind-fd feature, then a real user+mount namespace. A hung
# check fails the step; it is never retried.
probe() {
  local help probe_started=$SECONDS rc=0 result=0
  say "probe $1: start"
  : >"$probe_err"
  if [ ! -x /usr/bin/bwrap ]; then
    echo "trusted Bubblewrap not available at /usr/bin/bwrap" >"$probe_err"
    say "probe $1: end after $((SECONDS - probe_started))s (no /usr/bin/bwrap)"
    return 1
  fi
  help="$(timeout --kill-after=2 10 /usr/bin/bwrap --help 2>&1)" || rc=$?
  if timed_out "$rc"; then
    fail "probe $1: bwrap --help timed out after 10s"
  fi
  if ! grep -Fq -- '--bind-fd' <<<"$help"; then
    echo "Bubblewrap does not support the required --bind-fd" >"$probe_err"
    say "probe $1: end after $((SECONDS - probe_started))s (no --bind-fd)"
    return 1
  fi
  timeout --kill-after=2 20 /usr/bin/bwrap --die-with-parent --new-session --unshare-all --share-net \
    --ro-bind / / -- /bin/true 2>"$probe_err" || result=$?
  probe_seconds=$((probe_seconds + SECONDS - probe_started))
  say "probe $1: end after $((SECONDS - probe_started))s (exit $result)"
  if timed_out "$result"; then
    fail "probe $1: the namespace probe timed out after 20s"
  fi
  return "$result"
}

if ! probe initial; then
  echo "Initial Bubblewrap probe failed: $(cat "$probe_err")"
  package_started=$SECONDS
  # Short network timeouts and no apt-internal retry: the one retry
  # is apt_phase's, so a stalled mirror costs one bounded wait twice.
  apt_options=(-o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20
    -o Acquire::Retries=0 -o DPkg::Lock::Timeout=30)
  apt_phase "apt-get update" 90 apt-get "${apt_options[@]}" update
  # The ephemeral runner needs the new sandbox binaries, not restarts
  # of unrelated image services (PHP's restart previously took 90s).
  # needrestart's supported list-only mode applies to this install;
  # the real namespace probe below still judges the installed binary.
  apt_phase "apt-get install" 120 env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l \
    apt-get "${apt_options[@]}" install --yes --no-install-recommends bubblewrap apparmor-profiles
  package_seconds=$((SECONDS - package_started))
  package_started=""
else
  echo "Trusted Bubblewrap already supports --bind-fd and real namespaces; package preparation unnecessary"
fi

if probe prepared; then
  echo "Bubblewrap probe passed without an AppArmor profile"
else
  detail="$(cat "$probe_err")"
  echo "Bubblewrap probe failed: ${detail}"
  case "$detail" in
    *"setting up uid map: Permission denied"*) ;;
    *) fail "probe failed for a reason the packaged AppArmor profile does not address: ${detail}" ;;
  esac
  [ -f "$profile_src" ] || fail "apparmor-profiles did not provide $profile_src"
  if bwrap_profile_loaded; then
    fail "a bwrap AppArmor profile is already loaded but the probe fails; refusing to replace it"
  fi
  [ ! -e "$profile_dst" ] || fail "$profile_dst already exists; refusing to overwrite it"
  must_phase "install AppArmor profile" 10 --root install -m 0644 "$profile_src" "$profile_dst"
  must_phase "load AppArmor profile" 30 --root /usr/sbin/apparmor_parser -r "$profile_dst"
  bwrap_profile_loaded \
    || fail "AppArmor did not report the packaged bwrap profile as loaded"
  probe reloaded || fail "probe still fails after loading the packaged bwrap profile: $(cat "$probe_err")"
  echo "Bubblewrap probe passed after loading the packaged bwrap-userns-restrict profile"
fi
