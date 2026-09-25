import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://orbit-cli.com',
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
        'Reference documentation for Orbit, a self-hosted runtime for fleets of coding agents.',
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
            { slug: 'how-to/task-lifecycle', label: 'Run a Task Lifecycle' },
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
            content: 'https://orbit-cli.com/og-image.svg',
          },
        },
        {
          tag: 'meta',
          attrs: {
            property: 'og:image:type',
            content: 'image/svg+xml',
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
            name: 'twitter:card',
            content: 'summary_large_image',
          },
        },
        {
          tag: 'meta',
          attrs: {
            name: 'twitter:image',
            content: 'https://orbit-cli.com/og-image.svg',
          },
        },
      ],
    }),
  ],
});
