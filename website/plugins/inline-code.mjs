import { defineHastPlugin } from 'satteri';

// CSS nowrap keeps flags and identifiers atomic, while wbr still permits
// explicit breaks after spaces, path, key and assignment delimiters. Unlike
// zero-width characters, wbr adds nothing to selectable/copied code text.
//
// Each inline code sits in a nowrap span. The code is an inline-block that
// scrolls locally, and Chromium takes the white-space of an atomic inline's
// parent to decide whether a line may break after it. A nowrap parent keeps
// the punctuation that follows on the code's last line.
function segments(text) {
  return text.match(/\s+|[^\s/.=]*[/.=]|[^\s/.=]+/g) || [];
}

function wrapText(text) {
  return segments(text).flatMap((part) => {
    const node = { type: 'text', value: part };
    return /[\s/.=]$/.test(part)
      ? [node, { type: 'element', tagName: 'wbr', properties: {}, children: [] }]
      : [node];
  });
}

function wrapChildren(node) {
  return (node.children || []).flatMap((child) => {
    if (child.type === 'text') return wrapText(child.value);
    if (child.type === 'element') return [{ ...child, children: wrapChildren(child) }];
    return [child];
  });
}

function wrapRawHtml(html) {
  // Raw HTML examples share the same wrapping policy as Markdown. Leave pre
  // blocks and their syntax-highlighting markup intact, and preserve entities,
  // attributes and existing inline markup when wrapping text inside code.
  return html.split(/(<pre\b[\s\S]*?<\/pre\s*>)/gi).map((part, index) => {
    if (index % 2) return part;
    return part.replace(/(<code\b(?:"[^"]*"|'[^']*'|[^'">])*>)([\s\S]*?)(<\/code\s*>)/gi, (_match, open, content, close) => {
      const wrapped = content.split(/(<(?:"[^"]*"|'[^']*'|[^'">])*>)/g).map((text, textIndex) => {
        if (textIndex % 2) return text;
        return segments(text).map((segment) =>
          `${segment}${/[\s/.=]$/.test(segment) ? '<wbr>' : ''}`).join('');
      }).join('');
      return `<span class="${CODE_BOX_CLASS}">${open}${wrapped}${close}</span>`;
    });
  }).join('');
}

const CODE_BOX_CLASS = 'orbit-code-box';

function isCodeBox(node) {
  const className = node?.properties?.className;
  return node?.tagName === 'span' && [].concat(className || []).includes(CODE_BOX_CLASS);
}

export const inlineCodeWrap = defineHastPlugin({
  name: 'inline-code-wrap',
  raw(node, ctx) {
    if (typeof node.value !== 'string') return;
    const wrapped = wrapRawHtml(node.value);
    if (wrapped !== node.value) ctx.replaceNode(node, { type: 'raw', value: wrapped });
  },
  element: {
    filter: ['code'],
    visit(node, ctx) {
      for (let parent = ctx.parent(node); parent; parent = ctx.parent(parent)) {
        if (parent.tagName === 'pre') return;
      }
      if (isCodeBox(ctx.parent(node))) return;
      ctx.replaceNode(node, {
        type: 'element',
        tagName: 'span',
        properties: { className: [CODE_BOX_CLASS] },
        children: [{ ...node, children: wrapChildren(node) }],
      });
    },
  },
});
