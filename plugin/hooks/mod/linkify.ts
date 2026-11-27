// Ported from the personal task-links mod. Only inline Markdown prose is rewritten.

const TASK_PREFIX = /^[A-Z]{2,5}$/
const RUN_ID = /jrun-\d{8}-\d{4}(?:-[ct]?\d+)?/.source
const MAX_DRAWN = 100_000
const FENCE = /^(`{3,}(?=[^`]*$)|~{3,})/
const QUOTE = /^ {0,3}> ?/
const LIST_ITEM = /^([-+*]|\d{1,9}[.)])([ \t]+|$)/
const HEADING = /^#{1,6}([ \t]|$)/
const RULE = /^([-*_])([ \t]*\1){2,}[ \t]*$|^=+[ \t]*$/
const DEFINITION = /^\[((?:\\.|[^\]\\^])(?:\\.|[^\]\\])*)\]:[ \t]*\S/
// Anything with a scheme, `www.` addresses and `mailto:`: the surface links these itself.
const BARE_URL = /(?:\b[a-z][a-z0-9+.-]*:\/\/|\bwww\.|\bmailto:)[^\s<`]*/iy
const ANCHOR = /<a(?=[\s>])[\s\S]*?(?:<\/a\s*>|$)/iy
const TAG = /<!--[\s\S]*?(?:-->|$)|<(?:[a-z][a-z0-9+.-]*:[^\s<>]*|[^\s<>@]+@[^\s<>]+|\/?[a-z][a-z0-9-]*(?:\s(?:"[^"]*"|'[^']*'|[^<>"'])*)?\/?)>/iy
const ANGLE_DESTINATION = /<(?:\\[^\n]|[^<>\\\n])*>/y
const LINK_END = /(?:\s+(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\((?:\\.|[^()\\])*\)))?\s*\)/y

type Range = [start: number, end: number]

const label = (text: string) => text.replace(/\s+/g, ' ').trim().toLowerCase()

/**
 * The stretches of `text` that hold inline markdown (a paragraph, a heading, a
 * table cell), code blocks and link definitions left out.
 */
function inlineBlocks(text: string): { blocks: Range[]; definitions: Set<string> } {
  const blocks: Range[] = []
  const definitions = new Set<string>()
  let open: Range | null = null
  let fence: { marker: string; quotes: number } | null = null
  // Content columns of the list items the current line may continue.
  let lists: number[] = []
  let quotes = 0
  const close = () => {
    if (open) blocks.push(open)
    open = null
  }

  let offset = 0
  for (const line of text.split('\n')) {
    const start = offset
    const end = start + line.length
    offset = end + 1

    let rest = line
    let depth = 0
    for (let quote = QUOTE.exec(rest); quote; quote = QUOTE.exec(rest)) {
      rest = rest.slice(quote[0].length)
      depth++
    }
    const body = rest.trimStart()
    const indent = rest.slice(0, rest.length - body.length).replace(/\t/g, '    ').length

    if (fence) {
      if (depth >= fence.quotes) {
        const closer = /^(`+|~+)[ \t\r]*$/.exec(body)?.[1]
        if (closer && closer[0] === fence.marker[0] && closer.length >= fence.marker.length) fence = null
        continue
      }
      fence = null
    }

    if (depth !== quotes) {
      // A line with fewer markers may lazily continue a quoted paragraph.
      if (depth > quotes || body === '') {
        close()
        lists = []
      }
      quotes = depth
    }

    if (body === '') {
      close()
      continue
    }

    const isItem = LIST_ITEM.exec(body)
    if (!open || isItem) {
      while (true) {
        const top = lists[lists.length - 1]
        if (top === undefined || indent >= top) break
        lists.pop()
      }
    }
    const base = lists[lists.length - 1] ?? 0

    // Indented code cannot interrupt a paragraph.
    if (!open && indent >= base + 4) continue

    let content = body
    const isRule = RULE.test(body)
    if (isItem && !isRule && indent < base + 4) {
      close()
      const [full, marker = '', whitespace = ''] = isItem
      const gap = whitespace.length
      lists.push(indent + marker.length + (gap >= 1 && gap <= 4 ? gap : 1))
      content = body.slice(full.length)
      if (gap > 4) continue
    }

    const opener = FENCE.exec(content)?.[1]
    if (opener) {
      close()
      fence = { marker: opener, quotes: depth }
      continue
    }
    if (isRule) {
      close()
      continue
    }

    const from = end - content.length
    const definition = open ? null : DEFINITION.exec(content)
    if (definition) {
      const [, name = ''] = definition
      definitions.add(label(name))
      continue
    }

    if (content.startsWith('|')) {
      // Cells are split before inline parsing: a stray backtick stays in its cell.
      close()
      let cell = from
      for (let index = from; index < end; index++) {
        if (text[index] === '\\') index++
        else if (text[index] === '|') {
          blocks.push([cell, index])
          cell = index + 1
        }
      }
      blocks.push([cell, end])
    } else if (HEADING.test(content)) {
      close()
      blocks.push([from, end])
    } else if (open) {
      open[1] = end
    } else {
      open = [from, end]
    }
  }
  close()

  return { blocks, definitions }
}

/** The index after the code span, or null when its backticks have no partner. */
function codeSpanEnd(text: string, start: number): { run: number; end: number | null } {
  let run = 1
  while (text[start + run] === '`') run++
  const closer = /`+/g
  closer.lastIndex = start + run
  for (let match = closer.exec(text); match; match = closer.exec(text)) {
    if (match[0].length === run) return { run, end: match.index + run }
  }

  return { run, end: null }
}

/** The index of the matching closing bracket, or -1. */
function closingBracket(text: string, start: number): number {
  let depth = 0
  for (let index = start; index < text.length; index++) {
    const character = text[index]
    if (character === '\\') index++
    else if (character === '`') {
      const span = codeSpanEnd(text, index)
      index = (span.end ?? index + span.run) - 1
    } else if (character === '[') depth++
    else if (character === ']' && --depth === 0) return index
  }

  return -1
}

/** The index after the `(destination "title")`, or -1. */
function destinationEnd(text: string, start: number): number {
  let index = start + 1
  while (/\s/.test(text[index] ?? '')) index++
  if (text[index] === '<') {
    ANGLE_DESTINATION.lastIndex = index
    if (!ANGLE_DESTINATION.test(text)) return -1
    index = ANGLE_DESTINATION.lastIndex
  } else {
    let depth = 0
    for (; index < text.length; index++) {
      const character = text.charAt(index)
      if (/\s/.test(character) || (character === ')' && depth === 0)) break
      if (character === '\\' && /[!"#$%&'()*+,\-./:;<=>?@[\]\\^_`{|}~]/.test(text[index + 1] ?? '')) {
        index++
      } else if (character === '(') depth++
      else if (character === ')') depth--
    }
    if (depth !== 0) return -1
  }

  LINK_END.lastIndex = index
  return LINK_END.test(text) ? LINK_END.lastIndex : -1
}

/** The stretches of one inline block that are code, links or addresses. */
function protectedRanges(block: string, definitions: Set<string>): Range[] {
  const ranges: Range[] = []
  const matchAt = (pattern: RegExp, index: number) => {
    pattern.lastIndex = index

    return pattern.test(block) ? pattern.lastIndex : -1
  }

  for (let index = 0; index < block.length; ) {
    const character = block.charAt(index)
    let end = -1

    if (character === '\\') {
      index += 2
      continue
    }
    if (character === '`') {
      const span = codeSpanEnd(block, index)
      if (span.end === null) {
        index += span.run
        continue
      }
      end = span.end
    } else if (character === '[') {
      const close = closingBracket(block, index)
      if (close !== -1) {
        const text = block.slice(index + 1, close)
        if (block[close + 1] === '(') end = destinationEnd(block, close + 1)
        if (end === -1 && block[close + 1] === '[') {
          const second = closingBracket(block, close + 1)
          if (second !== -1 && definitions.has(label(block.slice(close + 2, second) || text))) end = second + 1
        }
        if (end === -1 && definitions.has(label(text))) end = close + 1
        // Plain brackets, as in "[ORB-1]", are text: scan inside them.
      }
    } else if (character === '<') {
      end = matchAt(ANCHOR, index)
      if (end === -1) end = matchAt(TAG, index)
    } else {
      end = matchAt(BARE_URL, index)
    }

    if (end === -1) {
      index++
    } else {
      ranges.push([index, end])
      index = end
    }
  }

  return ranges
}

export function createLinkifier(prefixes: readonly string[], origin: string): (text: string) => string {
  const registered = [...new Set(prefixes.filter(prefix => TASK_PREFIX.test(prefix)))].sort(
    (left, right) => right.length - left.length,
  )
  // A malformed run suffix must not leave a link to just the legacy stem.
  const ids = [`${RUN_ID}(?!-\\w)`]
  if (registered.length > 0) ids.push(`(?:${registered.join('|')})-\\d+`)
  const pattern = new RegExp(`\\b(?:${ids.join('|')})\\b`, 'g')

  return text => linkify(text, pattern, origin)
}

function linkify(text: string, pattern: RegExp, origin: string): string {
  if (text.length > MAX_DRAWN || !pattern.test(text)) {
    pattern.lastIndex = 0
    return text
  }
  pattern.lastIndex = 0

  const { blocks, definitions } = inlineBlocks(text)
  let out = ''
  let copied = 0

  for (const [from, to] of blocks) {
    const block = text.slice(from, to)
    const ids = [...block.matchAll(pattern)]
    if (ids.length === 0) continue

    const skip = protectedRanges(block, definitions)
    let rangeIndex = 0
    for (const match of ids) {
      const at = match.index
      let range = skip[rangeIndex]
      while (range && range[1] <= at) {
        rangeIndex++
        range = skip[rangeIndex]
      }
      if (range && range[0] < at + match[0].length) continue

      const id = match[0]
      const route = id.startsWith('jrun-') ? 'r' : 't'
      out += text.slice(copied, from + at)
      // "!" before a new link would turn it into an image.
      if (out.endsWith('!') && !out.endsWith('\\!')) out = `${out.slice(0, -1)}\\!`
      out += `[${id}](<${origin}/${route}/${id}>)`
      copied = from + at + id.length
      if (out.length + text.length - copied > MAX_DRAWN) return text
    }
  }

  return out + text.slice(copied)
}
