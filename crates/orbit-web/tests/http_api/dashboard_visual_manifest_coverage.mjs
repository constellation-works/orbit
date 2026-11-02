// Every top-level dashboard view has a view-manifest entry, so the UI rules
// (scripts/ux-rules) render it, or a recorded exemption that says why not.
// The Notices view shipped without an entry and its section-title inset was
// never measured (ORB-21637). A top-level view is a `section.tab-pane` in the
// shipped index.html; a manifest view covers the pane its route's first hash
// segment names (`#analytics/metrics` covers `analytics`); `#runs/<id>` is the
// run-detail pane's route.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { parseManifest } from '../../../../scripts/ux-rules/lib/manifest.mjs';

// Top-level views the manifest does not render, each with its reason. An
// exemption that is no longer needed (the view gained an entry or was removed)
// fails, so the list cannot go stale.
const EXEMPTIONS = {
  leases: 'Fixture workspaces run with path leases off, so the view only shows its neutral off state over by-design 400 lease reads; it needs a leases-enabled fixture before the rules can measure it.',
};

const ROUTE_PANES = { runs: 'run-detail' };

const indexPath = new URL('../../assets/dashboard/index.html', import.meta.url);
const manifestPath = new URL('../../visual/views.toml', import.meta.url);

function topLevelPanes(html) {
  const panes = new Set();
  for (const match of html.matchAll(/<section\b([^>]*)>/g)) {
    const attrs = match[1];
    const classes = /\bclass="([^"]*)"/.exec(attrs)?.[1].split(/\s+/) ?? [];
    const tab = /\bdata-tab="([^"]+)"/.exec(attrs)?.[1];
    if (classes.includes('tab-pane') && tab) panes.add(tab);
  }
  return panes;
}

function coveredPanes(manifest) {
  const covered = new Set();
  for (const view of manifest.views) {
    if (view.app !== 'dashboard') continue;
    const top = /^#([^/?]+)/.exec(view.route)?.[1];
    if (top) covered.add(ROUTE_PANES[top] ?? top);
  }
  return covered;
}

function coverageProblems(panes, manifest, exemptions) {
  const covered = coveredPanes(manifest);
  const problems = [];
  for (const pane of panes) {
    const reason = exemptions[pane];
    if (covered.has(pane)) {
      if (reason !== undefined) problems.push(`${pane}: exempted but has a manifest entry; drop the exemption`);
    } else if (reason === undefined) {
      problems.push(`${pane}: no views.toml entry and no recorded exemption`);
    } else if (typeof reason !== 'string' || reason.trim().length < 20) {
      problems.push(`${pane}: exemption needs a reason`);
    }
  }
  for (const pane of Object.keys(exemptions)) {
    if (!panes.has(pane)) problems.push(`${pane}: exempted but no such dashboard view`);
  }
  return problems;
}

const html = fs.readFileSync(indexPath, 'utf8');
const manifestText = fs.readFileSync(manifestPath, 'utf8');
const panes = topLevelPanes(html);
const manifest = parseManifest(manifestText, manifestPath.pathname);

// The shipped dashboard must yield its views, or the guard below proves nothing.
assert.ok(panes.size >= 5 && panes.has('tasks') && panes.has('notices'), `top-level views not found: ${[...panes]}`);

const problems = coverageProblems(panes, manifest, EXEMPTIONS);
assert.deepEqual(
  problems,
  [],
  `Dashboard views the UI rules never render. Add a [[view]] to crates/orbit-web/visual/views.toml or record an exemption with its reason in this file:\n${problems.join('\n')}`,
);

// The guard reports a view that loses its entry, and an exemption with no reason.
const withoutNotices = { ...manifest, views: manifest.views.filter((view) => !view.route.startsWith('#notices')) };
assert.deepEqual(
  coverageProblems(panes, withoutNotices, EXEMPTIONS),
  ['notices: no views.toml entry and no recorded exemption'],
);
assert.deepEqual(
  coverageProblems(panes, withoutNotices, { ...EXEMPTIONS, notices: '' }),
  ['notices: exemption needs a reason'],
);
assert.deepEqual(
  coverageProblems(panes, manifest, { ...EXEMPTIONS, notices: 'A reason long enough to count as one.' }),
  ['notices: exempted but has a manifest entry; drop the exemption'],
);
