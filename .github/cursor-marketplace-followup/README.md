# Cursor marketplace follow-up acknowledgements

These files record that a maintainer submitted or updated the Cursor
marketplace listing for a specific Orbit version. They do **not** mean the
curated catalog is live, and they are not part of GitHub Release, Homebrew, or
npm publication.

After completing the procedure in `docs/runbooks/release.md`, add
`<version>.ack` to the maintained `agent-main` branch with:

```
version=<version>
```

Example: `0.19.0.ack` containing `version=0.19.0`.

The release tag remains immutable and cannot contain a receipt made after it
was published. After the receipt lands on `agent-main`, re-run the Cursor
marketplace follow-up job for that release tag. The job reads the maintained
branch's receipt while checking the tag's version, so it can clear the reminder
without retagging or republishing anything.
