import { defineHastPlugin } from 'satteri';
import releaseDates from '../src/data/release-dates.json' with { type: 'json' };

const changelogURL = new URL('../../CHANGELOG.md', import.meta.url).href;
const isChangelog = (ctx) => ctx.fileURL?.href === changelogURL;

// Only transform the imported release notes, leaving documentation examples
// and existing links alone. Public labels follow AGENTS.md's ID policy.
export const changelogLinks = defineHastPlugin({
  name: 'changelog-pull-requests',
  text(node, ctx) {
    if (!isChangelog(ctx)) return;
    for (let parent = ctx.parent(node); parent; parent = ctx.parent(parent)) {
      if (['a', 'code', 'pre'].includes(parent.tagName)) return;
    }
    const children = [];
    let end = 0;
    for (const match of node.value.matchAll(/\[([A-Z][A-Z0-9]{1,15}-\d{1,8})\]/g)) {
      children.push({ type: 'text', value: node.value.slice(end, match.index) });
      children.push({
        type: 'element',
        tagName: 'a',
        properties: {
          href: `https://github.com/constellation-works/orbit/pulls?q=${encodeURIComponent(`is:pr is:merged "${match[1]}"`)}`,
        },
        children: [{ type: 'text', value: 'Pull request' }],
      });
      end = match.index + match[0].length;
    }
    if (!end) return;
    children.push({ type: 'text', value: node.value.slice(end) });
    return { type: 'element', tagName: 'span', properties: {}, children };
  },
});

// A factory keeps the expanded-release counter local to each compilation.
export function changelogReleases() {
  let releaseIndex = 0;
  return defineHastPlugin({
    name: 'changelog-releases',
    element: {
      filter: ['h2'],
      visit(node, ctx) {
        if (!isChangelog(ctx)) return;
        const match = ctx.textContent(node).match(/^(\d+\.\d+\.\d+)(?:\s+[—-]\s+(\d{4}-\d{2}-\d{2}))?$/);
        if (!match) return;
        const [, version, recordedDate] = match;
        const date = recordedDate ?? releaseDates[version];
        const parsedDate = new Date(`${date}T00:00:00Z`);
        if (!date || Number.isNaN(parsedDate.getTime()) || parsedDate.toISOString().slice(0, 10) !== date) {
          throw new Error(`Release ${version} needs an ISO date in its CHANGELOG.md heading (see RELEASING.md).`);
        }
        const parent = ctx.parent(node);
        const index = ctx.indexOf(node);
        if (parent?.type !== 'root' || index === undefined) return;
        const body = [];
        for (const sibling of parent.children.slice(index + 1)) {
          if (sibling.type === 'element' && sibling.tagName === 'h2') break;
          // Materialize a copy before removing the original arena node.
          body.push(JSON.parse(JSON.stringify(sibling)));
          ctx.removeNode(sibling);
        }
        const latest = releaseIndex === 0;
        const open = releaseIndex++ < 3;
        return {
          type: 'element',
          tagName: 'details',
          properties: { className: ['orbit-release'], open },
          children: [
            {
              type: 'element',
              tagName: 'summary',
              properties: {},
              children: [{
                type: 'element',
                tagName: 'h2',
                // Preserve the version-only slug even once headings have dates.
                properties: { id: version.replaceAll('.', ''), className: ['orbit-release-heading'] },
                children: [
                  { type: 'text', value: `${version} ` },
                  {
                    type: 'element', tagName: 'time',
                    properties: { dateTime: date },
                    children: [{ type: 'text', value: date }],
                  },
                  ...(latest ? [{ type: 'text', value: ' ' }, {
                    type: 'element', tagName: 'span',
                    properties: { className: ['orbit-release-latest'] },
                    children: [{ type: 'text', value: 'Latest' }],
                  }] : []),
                ],
              }],
            },
            { type: 'element', tagName: 'div', properties: { className: ['orbit-release-body'] }, children: body },
          ],
        };
      },
    },
  });
}
