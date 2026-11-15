# Why — STD-05 Security boundaries

Why each rule in [STD-05](STD-05.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-05.md`; this file explains them and adds none.

Evidence below cites Orbit tasks (`ORB-`) and frictions (`F`), both in the ws_orbit store.
`D:<area>:<n>` means line *n* of `docs/design/<area>/4_decisions.md` in the Orbit repo. The
candidates were extracted and accepted in
`projects/standards/tacit-extraction-2026-09-26.md` (disposition recorded on ORB-13088);
each rule names the candidate it came from.

**R1.** *Candidate T14.* A value the subject can rewrite cannot tell the host who the subject
is. Orbit had a run of these:
- ORB-12775: the plugin tool allowlist was enforced through `ORBIT_ALLOWED_TOOLS`, which the
  sandboxed child could simply unset.
- ORB-12789: the first fix moved the gate to `ORBIT_PLUGIN`, another variable the child
  controls, and the gate was skipped on the MCP entry point.
- ORB-12798: the fallback identity used pid, parent pid and process group. A descendant that
  called `setsid` matched nothing the host had recorded, and its identity vanished.
- ORB-12768 and ORB-11607: privilege-bearing variables reached children that could then
  present them back.

The fix that held gives the backend a descriptor on a session record the host wrote, in a
directory the plugin cannot write. A child can close that descriptor, but it cannot forge
one. D:federated-mcp:203 states the principle: "an authorization statement that the actor can
rewrite is documentation, not a boundary." D:distributed-drain:166 makes the same point
about payloads: "a payload that names its own host can name any host." D:policy-sandbox:264
adds that a sandbox binary found earlier on `PATH` must be ignored, so the boundary does not
depend on inherited environment ordering.

**R2.** *Candidate T14.* Clients legitimately describe themselves, and that is useful in an
audit trail. D:auditability:580 records the decision to "add a second field rather than
relaxing the boundary". The authenticated `role` keeps its meaning, and a nullable
`self_reported_actor` column holds whatever the client claims. Merging the two would let any
client write its preferred identity into the trusted field.

**R3.** *Candidate S7.* D:operations-as-data:154: hiding destructive tools from the MCP
listing was "advertisement, not enforcement". CLI subcommands still reached the same tools
through an admin bypass. D:operations-as-data:211 makes the split explicit: "Placement is not
permission." Where an operation is listed is an audience decision. One governed-operations
registry, resolved at one chokepoint per surface, is the only authorization statement.

**R4.** *Candidate S7.* D:operations-as-data:170: when authority is ambiguous, the resolution
fails closed "with one explicit escape hatch". Orbit's `ORBIT_OPERATOR` override is
deliberately easy to set. It exists to make a deliberate act look deliberate, not to stop a
determined caller. Every use is audited with its own provenance, so the trail shows an
override and not an ordinary operator session.

**R5.** *Candidate S7.* D:operations-as-data:158: an authorization layer between processes
of the same OS user is "an **accident guard, not a security boundary**". Any agent on the box
can bypass it with `git`, `rm` or a direct write. Adding a password or token to it "buys no
protection and invites relaxing the surrounding rails on the strength of a boundary that does
not exist." D:federated-mcp:203 gives the same reason for the dashboard. Labelling the guard
honestly keeps reviewers from pricing it as a boundary.

**R6.** *Candidate S1.* A check on the spelled path answers a question about names while the
kernel acts on inodes:
- ORB-12799: a granted write root whose missing tail sat below a symlink could not be
  canonicalized. The check fell back to a name-only reading and bypassed the protected-root
  guard.
- ORB-12788: a write grant could name any child of the global root, reopening directories
  other guards relied on being unwritable. The allowlist, not the grant, decides.
- ORB-11506: sandbox rules had to deny a rewrite of `.git` reached through leaf paths.
- F2026-09-098: an alert on a credential-file read stayed open because the read still took a
  caller-influenced path despite earlier canonicalization fixes.

Orbit's `SECURITY.md` states the resulting policy: canonicalize the target, or the nearest
existing ancestor for a new file, and deny "a symlink from an allowed tree into a denied
one."

**R7.** *Candidate S1.* A correct check does not help when the operation resolves the path
again, differently:
- ORB-12971: rule injection wrote through agent-guide symlinks to files outside the
  workspace.
- ORB-12800: plugin removal followed the hostile `install_path` that the loader had just
  refused.
- ORB-12799: the fix makes validation and enforcement share one resolution
  (`physical_with_missing_tail`). Creating the root refuses when what was created does not
  resolve to the validated identity.

`SECURITY.md` also records the gap no path check closes: the time between check and use. A
directory an attacker can change concurrently is untrusted.

**R8.** *Candidate P5.* ORB-11032: Orbit created its SQLite stores and state directories
with plain `create_dir_all` and `Connection::open`, so their modes came from the process
umask. The reviewed live install had a `775` state root and `644` databases and sidecars,
readable by every local user, while the main database held bearer claim tokens. The fix
creates files `0600` and directories `0700` explicitly. It hardens the database before
SQLite can create its write-ahead-log sidecars, because sidecars created under a loose umask
leak the same data.

**R9.** *Candidate P5.* ORB-12450: `mcp-callers.toml`, the file that states who may call a
machine and with which capabilities, was seeded with plain `std::fs::write` under the ambient
umask and loaded with no owner or mode check. Anyone else who could write it could grant
themselves operator capability. The same codebase already refused a companion override that
was not owned by the effective user or was group- or world-writable, so the check existed and
was simply not applied to the file that mattered most. A file that grants authority is
an authority input under R1, so its integrity is checked when it is read, not only when it is
written. The Orbit module that fixed ORB-12450 has since been folded into another design.
Orbit's current load path hardens modes and refuses symlinks, and `orbit doctor` reports
group- or world-writable state directories.

**R10.** *Candidate S2.* A denylist admits every name nobody thought to forbid. A benignly
named credential such as `DATABASE_URL` or an internal service URL reaches the child, even
though the operator turned inheritance off. Orbit's `child_env` module notes that
credential-name and value-shape heuristics "cannot classify names an operator's environment
actually uses, and treating them as a gate is what let the bypass exist." ORB-11031 brought
`proc.spawn` under the same allowlist as agent subprocesses. `docs/DATA_HANDLING.md` adds that
Orbit's OS sandbox is a filesystem boundary, not a network boundary, so the environment
allowlist is the main control on exfiltration. Orbit's `docs/POSITIONING.md` lists
sandboxed-by-default execution as a non-negotiable.
*Python:* `subprocess.run(argv, env=composed_env)`, where `composed_env` is built from named
keys, never `os.environ.copy()` with deletions.

**R11.** *Candidate S2.* ORB-11607: an `ORBIT_*` prefix wildcard admitted Orbit's own
privilege-bearing variables into agent subprocesses. ORB-12768: a plugin manifest's
`env_pass` copied any parent variable by name, including `ORBIT_OPERATOR` and
`ORBIT_WORKSPACE_CLAIM_TOKEN`. A pass list can come from an untrusted source, such as a plugin
manifest, so the exclusion holds "regardless of who is asking."

**R12.** *Candidate S2.* On Linux and macOS, another process of the same user can read a
process's argv, and often its environment. ORB-11134 moved SSH MCP acceptance bearers out of
same-UID process metadata. ORB-11184 found the capability still travelled in an environment
variable: the process sealed itself (`PR_SET_DUMPABLE=0`) only once `main` ran, and a sibling
process polling `/proc/*/environ` recovered the capability in the window before that. A secret in an inherited environment also reaches every grandchild, which is
the R10 problem again.

**R13.** *Candidate S3.* D:auditability:84 decides to redact secrets "before durable blob or
error-message persistence." Redacting at display time leaves the secret on disk. ORB-10958:
the artifact redaction allowlist missed `session_log.append` and other writers of persisted
free text, so those writers stored unredacted text. Orbit now keys its artifact redaction by
action. An explicit inventory, checked by a test, is what makes a missing writer visible.
`docs/DATA_HANDLING.md` is clear about the limits: "Redaction is a safety net, not the
boundary — the allowlist is the boundary." R10 to R12 keep secrets out, and this rule catches
what slips through.

**R14.** *Candidate S3.* F2026-08-081: a variable with a secret-bearing name held an ordinary
English word. Value matching then replaced every plain-English use of that word in a stored
task summary with `[REDACTED_ENV]`, corrupting the record. ORB-12084: an unanchored provider-key
pattern matched inside an ordinary hyphenated identifier and truncated it mid-word in a
stored task. Over-redaction destroys the audit trail that
redaction is meant to protect, and it teaches operators to turn redaction off. Orbit
therefore skips values shorter than four characters and a small set of ordinary words, and it
anchors credential patterns so they do not match inside identifiers.

**R15.** *Source: Orbit positioning, not a tacit candidate.* `docs/POSITIONING.md` lists
"Bring-your-own-credentials" as a non-negotiable: "API keys belong to the operator; Orbit is
pass-through." `docs/DATA_HANDLING.md`: "Orbit never stores a provider key in its own state."
A copy of a credential in the tool's state is one more place to leak from, and one more place
a rotation misses.

**R16.** *Candidate S4.* A browser on the same machine can reach a loopback socket. DNS
rebinding makes an attacker's hostname resolve to `127.0.0.1`, so the browser treats the
response as same-origin:
- ORB-12506: the dashboard never validated `Host`, so any website could read every API `GET`.
- ORB-12531: the first fix missed `/healthz?detailed=true`, which still leaked workspace
  names and host paths.

Browsers omit `Origin` on same-origin `GET`, so only a `Host` check stops a read.

**R17.** *Candidate S4.* Unsafe methods need an `Origin` check as well, because the classic
cross-site request forgery sends a cross-origin `POST` with the right `Host`. ORB-11613 fixed
Orbit's check to compare `Origin` against the request `Host`, and to accept `[::1]`, rather
than a prefix match. Both headers are client-supplied, and `curl` can set them to anything.
This is browser mitigation, not authentication. R16 and R17 together stop a web page, and the
loopback bind stops everything off the machine.

**R18.** *Candidate S4.* ORB-12929 and F2026-09-214: `orbit mcp listen` handed accepted
sockets straight to the protocol library. The library skipped lines that were not valid JSON,
so it skipped an HTTP request line and headers, then executed the JSON-RPC lines in the
`POST` body. Any web page could write tasks while the listener ran. The fix peeks at the
first byte and closes the connection unless it opens a JSON object.

**R19.** *Candidate S4.* D:remote-access:41: "Refuse every non-loopback Web bind" and reach a
remote dashboard through SSH, rather than adding dashboard credentials or a routable
listener. An SSH login already authenticates the operator. A new password system is new
attack surface that a small tool has no staff to maintain. This is a SHOULD because some
services exist to serve a network, and those record a deviation.

**R20.** *Source: Orbit positioning, not a tacit candidate.* `docs/POSITIONING.md`: "Orbit
must never phone home." `docs/DATA_HANDLING.md`: "no update check on startup, no crash
reporter, no usage analytics," and "no network call you cannot trace to a command you ran or
a crew you configured." An operator can only reason about data leaving the machine when every
request traces to something they did.

**R21.** *Candidate S5.* ORB-12767: the plugin loader never verified `manifest_digest`.
Grants recorded by name applied to whatever `plugin.yaml` was on disk, and audit rows
attested a digest that did not run. ORB-12803: plugin sync installed an unverified source
identity and reported a mismatched pin as satisfied. Consent given to one set of bytes has to
stop applying when the bytes change. Orbit now registers a plugin with a rewritten manifest
as inactive, and its diagnostic names both digests.

**R22.** *Candidate S5.* A name is a key only if it is unique:
- ORB-12804: colliding plugin definition names overwrote another plugin's seeded schedules.
- ORB-12787: a plugin's skill directory was linked into the provider's skills directory
  without a namespace, so a plugin with no grants replaced Orbit's own skill for every
  workspace on the host.
- ORB-12791: plugin tool names were validated against the manifest's `first_party` claim but
  registered from the store row, so a disagreeing row registered a plugin tool over a
  built-in.

**R23.** *Candidate S6.* ORB-11515: npm smoke CI ran an MCP publisher that was neither pinned
nor verified. ORB-12452: the website checks gate pinned `setup-node` to a commit that did not
exist, so every run failed at job setup and the gate enforced nothing. A pin has to be
verified to resolve, and a gate that cannot start is not a gate. Orbit pins CI actions by
SHA, checks downloaded CI tools against recorded SHA-256 sums, and verifies release
artifacts against a signed checksum manifest.

**R24.** *Candidate S6.* ORB-12684: vendored copies of DOMPurify and marked in the dashboard
had no dependency record, so no advisory service could see them. A dependency that nothing
records is one a published advisory never reaches. Orbit added a package manifest that
Dependabot watches, plus a vendor manifest whose digests CI checks against the served files.

**R25.** *Candidate S6.* ORB-11065: the SSH MCP acceptance token was the random part of a
temporary-file name, drawn from a non-cryptographic generator with a predictable seed. That
token was the only thing between an ordinary remote command and a forged MCP identity.
Temporary-name, hashing and simulation generators are built to be fast, not unguessable. *Rust:* `getrandom::fill`, or `rand::rngs::OsRng`. *Python:* the `secrets` module,
never `random`.
