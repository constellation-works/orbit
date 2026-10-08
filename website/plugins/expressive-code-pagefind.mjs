// Expressive Code renders a frame's chrome (its title, the screen-reader
// "Terminal window" label) as text in a figcaption beside the code. Pagefind
// would index that text as prose, so search excerpts read like sentences.
// Mark the caption so the index skips it and keeps only the code and copy.
function markCaptions(node) {
  if (node.type === 'element' && node.tagName === 'figcaption') {
    node.properties = { ...node.properties, 'data-pagefind-ignore': '' };
  }
  for (const child of node.children || []) markCaptions(child);
}

export const pagefindIgnoreFrameChrome = {
  name: 'pagefind-ignore-frame-chrome',
  hooks: {
    postprocessRenderedBlock({ renderData }) {
      markCaptions(renderData.blockAst);
    },
  },
};
