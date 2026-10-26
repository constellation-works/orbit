import { defineMdastPlugin } from 'satteri';
import { renderMermaidSVG } from 'beautiful-mermaid';

function escapeDiagramText(text) {
  return text.replace(/[&<>"']/g, (character) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[character]);
}

// Render before syntax highlighting. A factory keeps marker IDs unique within
// each document while leaving the authored fence as the single source of truth.
export function mermaidDiagrams() {
  let diagramIndex = 0;
  return defineMdastPlugin({
    name: 'static-mermaid-diagrams',
    code(node) {
      if (node.lang !== 'mermaid') return;
      const label = escapeDiagramText(node.meta || 'Dependency diagram');
      const prefix = `orbit-diagram-${++diagramIndex}-`;
      const svg = renderMermaidSVG(node.value, {
        font: 'Geist Variable',
        transparent: true,
        padding: 24,
      })
        // The renderer embeds Google Fonts and unscoped text/svg styles.
        // custom.css supplies scoped styles using the site's local font/tokens.
        .replace(/<style>[\s\S]*?<\/style>/g, '')
        .replace('<svg ', () => `<svg role="img" aria-label="${label}" `)
        .replace(/ id="([^"]+)"/g, ` id="${prefix}$1"`)
        .replace(/url\(#([^)]+)\)/g, `url(#${prefix}$1)`);

      return { rawHtml: `<figure class="orbit-diagram">
<figcaption>Scroll to explore the diagram. Expand the source for a text alternative.</figcaption>
<div class="orbit-diagram-viewport" role="region" aria-label="Scrollable diagram" tabindex="0">${svg}</div>
<details><summary>Diagram source (Mermaid)</summary><pre><code>${escapeDiagramText(node.value)}</code></pre></details>
</figure>` };
    },
  });
}
