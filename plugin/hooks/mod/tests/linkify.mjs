// Run with Node 22.18+.
import assert from 'node:assert/strict'
import { createLinkifier } from '../linkify.ts'

const HOST = 'https://orbit.example.test'
const defaults = ['ORB', 'ORBA']
const link = (id, route = 't', origin = HOST) => `[${id}](<${origin}/${route}/${id}>)`
const orb = number => link(`ORB-${number}`)
const orba = number => link(`ORBA-${number}`)

let count = 0
const is = (name, input, expected, prefixes = defaults, origin = HOST) => {
  assert.equal(createLinkifier(prefixes, origin)(input), expected, name)
  count++
}
const same = (name, input) => is(name, input, input)

is('plain text', 'See ORB-12 now.', `See ${orb(12)} now.`)
is('start and end of a line', 'ORB-1 blocks ORB-2\nORB-3', `${orb(1)} blocks ${orb(2)}\n${orb(3)}`)
is('ORBA vs ORB', 'ORBA-5 and ORB-5', `${orba(5)} and ${orb(5)}`)
same('no ids', 'nothing here, orb-1, XORB-1, ORB-1a, ORB-, ORBX-1')
same('unregistered prefix', 'DANI-7 stays')
is('registered prefix', 'DANI-7', link('DANI-7'), [...defaults, 'DANI'])
same('unrelated namespaces', 'STD-12 UTF-8 SHA-256')
is('empty registry', 'ORB-1', 'ORB-1', [])
is('invalid registry prefix', 'STD-12 ORB-1', `STD-12 ${orb(1)}`, ['ST.*', 'ORB'])

// inline code
is('inline code', 'run `orbit show ORB-1` for ORB-2', `run \`orbit show ORB-1\` for ${orb(2)}`)
same('double backtick span', '``a ` ORB-1``')
same('span across lines', '`orbit show\nORB-1`')
is('unmatched backtick', 'a ` b ORB-1', `a \` b ${orb(1)}`)
is('escaped backtick', '\\`ORB-1\\`', `\\\`${orb(1)}\\\``)
is('stray backtick does not cross paragraphs', 'a ` b\n\nORB-1 `x`', `a \` b\n\n${orb(1)} \`x\``)
is('stray backtick does not cross list items', '- a ` b\n- ORB-1 `x`', `- a \` b\n- ${orb(1)} \`x\``)
is('stray backtick stays in its table cell', '| a ` | `ORB-1` | ORB-2 |', `| a \` | \`ORB-1\` | ${orb(2)} |`)

// code blocks
is('fenced block', 'ORB-1\n```bash\norbit show ORB-2\n```\nORB-3', `${orb(1)}\n\`\`\`bash\norbit show ORB-2\n\`\`\`\n${orb(3)}`)
same('tilde fence', '~~~\nORB-1\n~~~')
same('longer fence holds shorter', '````\n```\nORB-1\n```\n````')
same('unclosed fence (streaming)', '```\nORB-1')
same('fence in a list item', '1. step\n\n    ```\n    ORB-1\n    ```')
same('fence on the marker line', '- ```\n  ORB-1\n  ```')
same('fence in a quote', '> ```\n> ORB-1\n> ```')
is('quote end closes its fence', '> ```\n> x\n\nORB-1', `> \`\`\`\n> x\n\n${orb(1)}`)
same('indented block', 'Run:\n\n    orbit show ORB-1\n\n\torbit show ORB-2')
same('indented block in a quote', '>     ORB-1')
same('indented block in a list item', '- item\n\n      ORB-1')
is('indented paragraph continuation', 'text\n    ORB-1', `text\n    ${orb(1)}`)
is('list continuation paragraph', '- item\n\n    about ORB-1', `- item\n\n    about ${orb(1)}`)
is('nested list', '- a\n    - ORB-1\n        - ORB-2', `- a\n    - ${orb(1)}\n        - ${orb(2)}`)
is('after a list, indentation is code again', '- a\n\ntext\n\n    ORB-1', '- a\n\ntext\n\n    ORB-1')

// links
same('link text', '[ORB-1](https://example.com)')
same('link text with more words', '[the **ORB-1** fix](https://example.com "ORB-2")')
same('link target', '[task](https://example.com/ORB-1)')
same('relative target', '[task](docs/ORB-1_(a).md)')
same('nested parentheses in link destination', '[ORB-1](docs/(sub(ORB-2)).md)')
same('image', '![ORB-1](https://x/ORB-2.png)')
same('nested brackets in link text', '[a [b] ORB-1](https://x)')
same('code in link text', '[`]` ORB-1](https://x)')
same('reference link', '[ORB-1][t] and [x ORB-2][]\n\n[t]: https://x/ORB-3\n[x ORB-2]: https://y')
same('shortcut reference', '[ORB-1]\n\n[ORB-1]: https://x')
same('bare URL', 'at https://x/?workspace=all#tasks/ORB-1 now')
same('www and file URLs', 'www.x.com/ORB-1 file:///tmp/ORB-2')
same('autolink', '<https://x/ORB-1>')
same('html anchor', '<a href="https://x/ORB-1">ORB-2</a>')
is('plain brackets (commit subjects)', 'fix: x [ORB-1]', `fix: x [${orb(1)}]`)
is('after a link', '[a](https://x) ORB-1', `[a](https://x) ${orb(1)}`)
is('unmatched bracket', '[ ORB-1', `[ ${orb(1)}`)
is('emphasis and heading', '## **ORB-1**', `## **${orb(1)}**`)
is('bang before id', 'done!ORB-1', `done\\!${orb(1)}`)
is('comparison is not a tag', 'a < b ORB-1 > c', `a < b ${orb(1)} > c`)
is('CRLF', '```\r\nORB-1\r\n```\r\nORB-2', `\`\`\`\r\nORB-1\r\n\`\`\`\r\n${orb(2)}`)
is('local dashboard address', 'ORB-1', link('ORB-1', 't', 'http://localhost:7878'), defaults, 'http://localhost:7878')

// idempotent: linked output is left alone
const linkify = createLinkifier(defaults, HOST)
const once = linkify('ORB-1, ORBA-2 [ORB-3]')
same('second pass', once)

// Run IDs share the Markdown exclusions, including when there is no registry.
const child = 'jrun-20261126-0531-c3'
const topLevel = 'jrun-20261126-0531-t12'
const legacyStem = 'jrun-20261126-0531'
const legacyNumbered = 'jrun-20261126-0531-2'
const runs = [child, topLevel, legacyStem, legacyNumbered]
is('current and legacy runs', runs.join(', '), runs.map(id => link(id, 'r')).join(', '))
is('registry failure preserves run linking', `ORB-1 ${child}`, `ORB-1 ${link(child, 'r')}`, [])
is('run in prose and code', `${child} \`${topLevel}\``, `${link(child, 'r')} \`${topLevel}\``)
same('run in an existing link and a bare URL', `[${child}](https://x/${child}) https://x/${topLevel}`)
same('malformed run IDs', `x${child} ${child}x ${legacyStem}-x3 ${legacyStem}-c ${legacyStem}0`)
is('run in a table cell', `| ${child} | ORB-1 |`, `| ${link(child, 'r')} | ${orb(1)} |`)
same('an exact run permalink stays exact', `[${child}](<${HOST}/r/${child}?machine=hm_example&workspace=demo>)`)
same('second pass of mixed output', linkify(`ORB-1 ${child}`))

const fits = `${'x'.repeat(99_999 - orb(1).length)} `
is('drawn text at the limit', `${fits}ORB-1`, `${fits}${orb(1)}`)
same('over the limit after rewriting', `${fits}ORB-1.`)
same('over the limit before rewriting', `ORB-1 ${'x'.repeat(100_000)}`)

// speed: a large message
const big = 'See ORB-12 and `ORB-13` [x](https://y).\n'.repeat(2000)
const began = performance.now()
linkify(big)
const ms = performance.now() - began

console.log(`ok: ${count} cases; ${big.length} chars in ${ms.toFixed(1)} ms`)
