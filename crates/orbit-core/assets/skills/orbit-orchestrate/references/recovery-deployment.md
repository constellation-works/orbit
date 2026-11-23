# Verify a repair on the deployed host

## Verify deployment separately from merge

When installation or service recovery is in scope, a merged source fix is
only an intermediate result. On the owning host:

1. Verify the executable actually invoked, service command, config, source
   revision, and checkout state. Preserve operator configuration overrides.
2. Build from the intended revision in an appropriate checkout, install to the
   actual executable location, and synchronize managed assets through the
   supported mechanism. Do not assume matching version strings mean matching
   binaries, or overwrite operator changes to make a source checkout clean.
3. Run `orbit doctor` after the upgrade and inspect its `workspace-convergence`
   and `routine-freshness` rows. `workspace-convergence` names registered
   workspaces whose managed artifacts still differ from the installed
   defaults; run `orbit workspace sync --all` on that host to converge them.
   Sync skips a workspace held by a live drain, so check it again after the
   drain releases the workspace.

   - A stale `routine-freshness` row can be the
     first sign that an enabled routine's jobs no longer load or succeed under
     the new build. Inspect the named routines with `orbit routine list`, then
     run `orbit clock tick` and confirm the expected jobs load and run.
4. Restart only the intended services. Verify their process/executable identity,
   sustained health and the repaired behavior; an immediate successful health
   response can miss a server that exits shortly afterward.
5. Record installed revision/hash, service result, and any remaining mismatch.
   Restore temporarily changed routines/settings unless the user made the
   change permanent.

See [maintenance.md](../../orbit-setup/references/maintenance.md) for supported
sync/upgrade mechanics. If capability or authority is missing, report it;
never create a shadow store or edit runtime state directly to bypass the gap.
