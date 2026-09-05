import { rm } from 'node:fs/promises';

// The retired scoreboard generator wrote untracked pages here. Removing that
// generator does not remove its output from existing checkouts, and Starlight
// would still publish those pages even without sidebar entries.
await rm(new URL('../src/content/docs/metrics/', import.meta.url), {
  recursive: true,
  force: true,
});
