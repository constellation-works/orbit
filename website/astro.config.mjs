import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { satteri } from '@astrojs/markdown-satteri';
import { defineHastPlugin } from 'satteri';
import { changelogLinks, changelogReleases } from './plugins/changelog.mjs';

function findFirstTh(node) {
  for (const child of node.children || []) {
    if (child.tagName === 'th') return child;
    const found = findFirstTh(child);
    if (found) return found;
  }
  return null;
}

function addClasses(node, ctx, ...classNames) {
  const current = node.properties?.className || node.properties?.class || [];
  const list = Array.isArray(current)
    ? [...current]
    : typeof current === 'string'
      ? current.split(/\s+/).filter(Boolean)
      : [];
  for (const name of classNames) {
    if (!list.includes(name)) {
      list.push(name);
    }
  }
  ctx.setProperty(node, 'className', list);
}

function visibleTextLength(html) {
  let length = 0;
  let index = 0;

  while (index < html.length) {
    if (html[index] === '<') {
      const tagEnd = html.indexOf('>', index + 1);
      if (tagEnd > index + 1) {
        index = tagEnd + 1;
        continue;
      }
    }

    const entity = html.startsWith('&lt;', index)
      ? '&lt;'
      : html.startsWith('&gt;', index)
        ? '&gt;'
        : html.startsWith('&quot;', index)
          ? '&quot;'
          : html.startsWith('&amp;', index)
            ? '&amp;'
            : null;
    if (entity) {
      length += 1;
      index += entity.length;
      continue;
    }

    length += 1;
    index += 1;
  }

  return length;
}

function processRawHtml(html) {
  const parts = html.split(/(<pre[\s\S]*?<\/pre>)/gi);
  for (let i = 0; i < parts.length; i += 2) {
    parts[i] = parts[i].replace(/<code([^>]*)>([\s\S]*?)<\/code>/gi, (match, attrs, content) => {
      if (visibleTextLength(content) > 38) {
        if (/class\s*=\s*["']/.test(attrs)) {
          attrs = attrs.replace(/class\s*=\s*(["'])([^"']*)\1/, (_m, q, cls) => {
            const classes = cls.split(/\s+/).filter(Boolean);
            if (!classes.includes('is-long')) classes.push('is-long');
            return `class=${q}${classes.join(' ')}${q}`;
          });
        } else {
          attrs = ` class="is-long"${attrs}`;
        }
        return `<code${attrs}>${content}</code>`;
      }
      return match;
    });
  }
  return parts.join('');
}

const inlineCodePlugin = defineHastPlugin({
  name: 'inline-code-wrap',
  raw(node, ctx) {
    if (typeof node.value === 'string' && node.value.includes('<code')) {
      const updated = processRawHtml(node.value);
      if (updated !== node.value) {
        ctx.replaceNode(node, { type: 'raw', value: updated });
      }
    }
  },
  element: {
    filter: ['code', 'table'],
    visit(node, ctx) {
      if (node.tagName === 'code') {
        const parent = ctx.parent(node);
        if (parent?.tagName === 'pre') {
          return;
        }
        const text = ctx.textContent(node);
        if (text.length > 38) {
          addClasses(node, ctx, 'is-long');
        }
      }

      if (node.tagName === 'table') {
        const firstTh = findFirstTh(node);
        if (firstTh && /^(options?|flags?)$/i.test(ctx.textContent(firstTh).trim())) {
          addClasses(node, ctx, 'sl-table-options', 'orbit-table-options');
        }

        const headerRow = node.children
          ?.find((child) => child.tagName === 'thead')
          ?.children?.find((child) => child.tagName === 'tr');
        const headers = headerRow?.children?.filter((child) => child.tagName === 'th') || [];
        if (
          headers.length === 2 &&
          ctx.textContent(headers[0]).trim() === 'Key' &&
          ctx.textContent(headers[1]).trim() === 'Type, default and purpose'
        ) {
          addClasses(node, ctx, 'orbit-config-keys');
        }
      }
    },
  },
});

export default defineConfig({
  site: 'https://orbit-cli.com',
  markdown: {
    processor: satteri({
      hastPlugins: [inlineCodePlugin, changelogLinks, changelogReleases],
    }),
  },
  vite: {
    // src/pages/changelog.astro imports the repository's CHANGELOG.md, one
    // directory above the site root; the dev server refuses that path unless
    // it is allow-listed. The build resolves it without this.
    server: { fs: { allow: ['..'] } },
  },
  integrations: [
    starlight({
      title: 'Orbit',
      expressiveCode: {
        styleOverrides: {
          borderRadius: '8px',
          codeBackground: 'var(--sl-color-bg-inline-code)',
          frames: {
            editorBackground: 'var(--sl-color-bg-inline-code)',
            terminalBackground: 'var(--sl-color-bg-inline-code)',
          },
        },
      },
      description:
        'Reference documentation for Orbit, a local-first runtime for coding agents.',
      logo: {
        dark: './src/assets/orbit-logo-dark.svg',
        light: './src/assets/orbit-logo-light.svg',
        alt: 'Orbit',
        replacesTitle: true,
      },
      favicon: '/favicon.svg',
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/constellation-works/orbit',
        },
      ],
      tableOfContents: {
        minHeadingLevel: 2,
        maxHeadingLevel: 3,
      },
      customCss: [
        '@fontsource-variable/geist',
        '@fontsource-variable/geist-mono',
        './src/styles/custom.css',
      ],
      components: {
        Header: './src/components/Header.astro',
        SiteTitle: './src/components/SiteTitle.astro',
        ThemeProvider: './src/components/ThemeProvider.astro',
        ThemeSelect: './src/components/ThemeSelect.astro',
        Footer: './src/components/Footer.astro',
        PageTitle: './src/components/PageTitle.astro',
      },
      pagefind: true,
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { slug: 'index', label: 'What Orbit Is' },
            { slug: 'getting-started', label: 'Quickstart' },
            { slug: 'getting-started/install', label: 'Install Orbit' },
            { slug: 'how-to/mcp-integration', label: 'Connect Your Agent' },
            { slug: 'getting-started/first-task', label: 'First Task' },
            { slug: 'getting-started/workflows', label: 'Delivery Workflows' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { slug: 'concepts', label: 'Overview' },
            { slug: 'concepts/tasks', label: 'Tasks' },
            { slug: 'concepts/agents', label: 'Agents and Crews' },
            { slug: 'concepts/activities-jobs', label: 'Activities and Jobs' },
            { slug: 'concepts/scheduling', label: 'Routines and Auto-Tasks' },
            { slug: 'concepts/policies', label: 'Policies' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { slug: 'how-to', label: 'Overview' },
            { slug: 'how-to/dashboard', label: 'Use the Dashboard' },
            { slug: 'how-to/continuous-delivery', label: 'Run a Delivery Window' },
            { slug: 'how-to/recurring-work', label: 'Schedule Recurring Work' },
            { slug: 'how-to/write-activity', label: 'Write an Activity' },
            { slug: 'how-to/scoping-rules', label: 'Choose Scopes' },
          ],
        },
        {
          label: 'Operate',
          items: [
            { slug: 'how-to/task-publication', label: 'Publish and Restore Tasks' },
            { slug: 'how-to/distributed-drain', label: 'Set Up a Distributed Drain' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { slug: 'reference', label: 'Overview' },
            { slug: 'reference/cli', label: 'CLI Commands' },
            { slug: 'reference/config', label: 'Configuration' },
            { slug: 'reference/activity-job-yaml', label: 'Activity and Job YAML' },
            { slug: 'reference/policy-format', label: 'Policy Format' },
            { slug: 'reference/scoping', label: 'Scoping Rules' },
          ],
        },
        {
          label: 'Project',
          items: [
            { label: 'Changelog', link: '/changelog/' },
            {
              label: 'Contributing',
              collapsed: true,
              items: [
                { slug: 'contributing', label: 'Overview' },
                { slug: 'contributing/local-dev', label: 'Local Development' },
                { slug: 'contributing/crate-layout', label: 'Crate Layout' },
                { slug: 'contributing/pr-workflow', label: 'PR Workflow' },
              ],
            },
            { slug: 'privacy', label: 'Privacy' },
          ],
        },
      ],
      head: [
        {
          tag: 'meta',
          attrs: {
            name: 'theme-color',
            content: '#0A0A0A',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image',
            content: 'https://orbit-cli.com/og-image.png',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:type',
            content: 'image/png',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:width',
            content: '1200',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:height',
            content: '630',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:alt',
            content: 'Orbit: a local-first runtime for coding agents, shown as three concentric orbits on a dark background.',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:card',
            content: 'summary_large_image',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image',
            content: 'https://orbit-cli.com/og-image.png',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image:alt',
            content: 'Orbit: a local-first runtime for coding agents, shown as three concentric orbits on a dark background.',
          },
        },
      ],
    }),
  ],
});
