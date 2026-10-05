// The newest released version, read from the repository's tracked
// CHANGELOG.md at build time so the header badge never drifts from the
// release notes. CHANGELOG.md is compiled at release time; its first H2 is
// the latest version.
import { getHeadings } from '../../../CHANGELOG.md';

export const latestRelease: string | undefined = getHeadings().find(
  (heading) => heading.depth === 2,
)?.text;
