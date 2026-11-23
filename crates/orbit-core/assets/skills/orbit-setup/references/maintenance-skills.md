# Maintenance: skill management

## Skill management

`orbit skill` works with two separate things on a host:

- **The catalog**: each shipped skill's files under the global Orbit root, by
  default `~/.orbit/skills/` (`orbit`, `orbit-orchestrate` and `orbit-setup`).
  `orbit workspace sync` reconciles these managed files and preserves local
  edits. A missing catalog entry is a sync question, not a `link` one.
- **The client links**: a symlink named after each shipped skill in
  `~/.agents/skills/` and `~/.claude/skills/`, each pointing at its catalog
  entry. Client CLIs discover skills through these links.

The plugin skill bundle is a third delivery path that these commands do not
touch; see [Managed resources and upgrades](maintenance-upgrades.md#managed-resources-and-upgrades).

| Subcommand | What it does | When to run it | How to verify |
|---|---|---|---|
| `orbit skill list` | Lists each installed skill with its ID, the first ten characters of its content hash, its tag count and its summary. | After an upgrade or sync, to confirm every shipped skill is installed. | The three shipped IDs appear, each with a hash. A hash that differs from the same skill on another host means the content differs. |
| `orbit skill show <name>` | Prints one skill's `Path`, full `Content hash`, its SKILL.md behavioral contract and its `meta.json` (or `(none)`). | To read what a skill tells agents to do, or to confirm which file a catalog entry resolves to. | The output starts with `Skill: <name>`, and the `Content hash` begins with the HASH that `list` shows. An unknown name fails with `skill not found` and exit status 1. |
| `orbit skill doctor [<name>]` | Without a name, checks each catalog skill and diagnoses dangling symlinks in existing client link directories. A dangling symlink in `~/.agents/skills/` or `~/.claude/skills/` is a `warning` that names its path. It does not report an expected client link that is absent. With a name, prints only that skill's findings. | After an upgrade, after an interrupted run, or to diagnose catalog problems and dangling client links. If a client cannot find a skill, also inspect the expected links under `~/.agents/skills/` and `~/.claude/skills/` or run `orbit skill link`. | A healthy doctor result ends with `All skills healthy.`, but only confirms the catalog checks and dangling-link checks it performs; it does not certify that every expected client link exists. With findings, the table lists them and `N issue(s) found.` goes to stderr. The exit status stays 0 for findings, so read the STATUS column. An unknown name fails with `no skill diagnostic named '<name>'` and exit status 1. |
| `orbit skill link` | Creates the client links for each shipped skill in `~/.agents/skills/` and `~/.claude/skills/`. It replaces an existing link that points elsewhere or dangles, and does nothing when the links are current. It fails if a shipped skill's catalog entry is missing, and it never creates catalog entries. | When an expected client link is absent, a client stops listing Orbit skills, or `doctor` reports a dangling link. | It prints `Linked N skill(s) in:` with both directories, or `Skill symlinks are already up to date.` when nothing changed. Then `ls -l ~/.agents/skills ~/.claude/skills` shows each shipped skill as a symlink to `~/.orbit/skills/<name>`. `orbit skill doctor` can confirm there are no reported catalog problems or dangling links, but a healthy result alone does not confirm every expected client link exists. |
| `orbit skill unlink` | Removes only the links that `link` creates: entries named after a shipped skill that point at its catalog entry. Plugin links, custom links, regular files and other symlinks stay in place, and a discovery `skills` directory left empty is removed. The catalog under `~/.orbit/skills/` is not changed. | When Orbit should stop offering the shipped skills to clients on this host, such as before moving to the plugin distribution or retiring the host. Clients stop finding them there until `orbit skill link` runs again. Confirm with the operator first, because it changes what clients load. | It prints `Removed N skill symlink(s).` or `No skill symlinks found to remove.`. Then `ls -l ~/.agents/skills ~/.claude/skills` shows no `orbit`-named links, and `orbit skill list` still lists the three skills. |

### Repair a missing or broken client link

1. Run `orbit skill doctor` to check the catalog and diagnose dangling links.
   It does not report an expected client link that is absent, so a healthy
   result does not prove every client can discover every shipped skill.
2. If a client cannot find a skill, inspect the expected links under
   `~/.agents/skills/` and `~/.claude/skills/`. Run `orbit skill link` to create
   absent links or repair links for shipped skills that point elsewhere or
   dangle.
3. If a shipped skill's catalog entry is missing, `orbit skill link` fails, so
   restore the catalog first with `orbit workspace sync` (see
   [Managed resources and upgrades](maintenance-upgrades.md#managed-resources-and-upgrades)), then
   confirm with `orbit skill list` and run `orbit skill link` again.
4. A dangling link named after something other than a shipped skill is not
   Orbit's to remove. Delete it by hand, as the `doctor` message says.
5. Verify expected links with `ls -l ~/.agents/skills ~/.claude/skills`, then
   run `orbit skill doctor` to confirm it reports no catalog problems or
   dangling links. Doctor still does not verify that all expected links exist.

Do not use `unlink` as a repair: it removes the links that clients use.
