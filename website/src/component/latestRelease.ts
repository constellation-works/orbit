// The newest released version, read from the repository's tracked
// CHANGELOG.md at build time so the header badge never drifts from the
// release notes. Skip non-version headings such as Unreleased.
import { getHeadings } from '../../../CHANGELOG.md';

export const latestRelease: string | undefined = getHeadings().find(
  (heading) => heading.depth === 2 && /^\d+\.\d+\.\d+/.test(heading.text),
)?.text.match(/^\d+\.\d+\.\d+/)?.[0];
