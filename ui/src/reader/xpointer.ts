// KOReader positions ↔ DOM positions in the section document foliate loads.
//
// KOReader (crengine) stores an EPUB position as an XPointer such as
//   /body/DocFragment[23]/body/p[1]/text()[1].0
//   /body/DocFragment[8]/body/div/p[9]/text().54
// The rules, read from crengine's source (koreader/crengine, lvtinydom.cpp
// at f335f7a, and lvxml.cpp) and CrossPoint's (lib/KOReaderSync at 5d18bee):
//
// - The root is crengine's own <body>; each spine item is one <DocFragment>
//   (all spine items since DOM 20240114, epubfmt.cpp), holding the item's
//   <body>. DocFragment[n] is the n-th spine item, 1-based.
// - Under <body>, each step is an element's local name and its 1-based index
//   among same-named element siblings (ldomXPointer::toStringV2). Older DOM
//   versions omit "[1]" when the element is the only one of its name; DOM
//   20260812+ always writes it. ParseXPathStep reads a missing index as
//   "the first". We read both and write "[n]" always (as CrossPoint does),
//   but leave <body> bare: CrossPoint's parser expects "/body/DocFragment[N]/body/".
// - text()[k] is the k-th TEXT node child, counted the same way (and
//   "text()" bare when it is the only one). Comments are not nodes in
//   crengine, and some whitespace-only text nodes the browser keeps are not
//   there (see crengineTextNodes).
// - ".N" is an offset in crengine's copy of the text: UTF-32 code points,
//   after the XML/HTML parser collapsed every run of space, tab, CR and LF to
//   one space (PreProcessXmlString; not inside white-space: pre*), and after
//   a <pre>'s leading newline was dropped. The browser keeps the raw text, so
//   offsets are mapped both ways (crOffsetToDom / domOffsetToCr).
// - Boxing elements crengine inserts (autoBoxing, floatBox, inlineBox,
//   tabularBox) are transparent in V2 XPointers; the source document has none.
//
// Known gaps, accepted: crengine decides "block" from its own CSS, we from
// the browser's computed style (or a tag list); a text run over 8192 chars is
// split into several crengine nodes (TEXT_SPLIT_SIZE); tabs in <pre> are
// expanded by crengine. Each only shifts a position by a little, and a
// pointer that does not resolve falls back to the percentage.

export interface XPointer {
  /** 0-based spine index (DocFragment[n] = n - 1) */
  spine: number
  /** element steps below the fragment's <body> */
  steps: { name: string; index: number }[]
  /** the text node and offset, when the pointer names one */
  text?: { index: number; offset: number }
  /** an element point (".N" right after an element step): a child index */
  point?: number
}

const STEP = /^([A-Za-z_][\w.:-]*|text\(\))(?:\[(\d+)\])?$/

/** Parse a crengine XPointer; null when it is not an EPUB body pointer. */
export function parseXPointer(s: string): XPointer | null {
  const m = /^\/body\/DocFragment(?:\[(\d+)\])?\/body(?:\[1\])?(\/.*)?$/.exec(s.trim())
  if (!m) return null
  const spine = (m[1] ? Number(m[1]) : 1) - 1
  if (spine < 0) return null
  const out: XPointer = { spine, steps: [] }
  let rest = m[2] ?? ''
  // a trailing ".N" belongs to the last step
  let pointOffset: number | null = null
  const pt = /\.(\d+)$/.exec(rest)
  if (pt) {
    pointOffset = Number(pt[1])
    rest = rest.slice(0, -pt[0].length)
  }
  const parts = rest.split('/').filter(Boolean)
  for (const [i, part] of parts.entries()) {
    const sm = STEP.exec(part)
    if (!sm) return null
    const index = sm[2] ? Number(sm[2]) : 1
    if (index < 1) return null
    if (sm[1] === 'text()') {
      if (i !== parts.length - 1) return null
      out.text = { index, offset: pointOffset ?? 0 }
      pointOffset = null
    } else {
      out.steps.push({ name: sm[1].toLowerCase(), index })
    }
  }
  if (pointOffset !== null) out.point = pointOffset
  return out
}

/** The canonical form we write (explicit indexes, bare body). */
export function formatXPointer(x: XPointer): string {
  let s = `/body/DocFragment[${x.spine + 1}]/body`
  for (const st of x.steps) s += `/${st.name}[${st.index}]`
  if (x.text) s += `/text()[${x.text.index}].${x.text.offset}`
  else if (x.point !== undefined) s += `.${x.point}`
  return s
}

/* ---------- crengine's view of the DOM ---------- */

const BLOCK_TAGS = new Set(
  'address article aside blockquote body center dd details dialog dir div dl dt fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 header hgroup hr li main menu nav ol p pre section summary table tbody thead tfoot tr td th caption ul'.split(
    ' ',
  ),
)

export interface Styling {
  /** is this element laid out as a block (crengine: display > inline) */
  isBlock(el: Element): boolean
  /** does this element keep its whitespace (white-space: pre*) */
  isPre(el: Element): boolean
}

/** Tag-based styling: what crengine's default epub.css says. Deterministic,
 * so a pointer written from one load reads back on the next. */
export const tagStyling: Styling = {
  isBlock: (el) => BLOCK_TAGS.has(el.localName.toLowerCase()),
  isPre: (el) => {
    for (let e: Element | null = el; e; e = e.parentElement) {
      const n = e.localName.toLowerCase()
      if (n === 'pre' || n === 'textarea') return true
    }
    return false
  },
}

/** The live document's computed style where it has one, else the tags. */
export function computedStyling(doc: Document): Styling {
  const view = doc.defaultView
  const style = (el: Element) => {
    try {
      return view?.getComputedStyle(el) ?? null
    } catch {
      return null
    }
  }
  return {
    isBlock(el) {
      const d = style(el)?.display
      if (!d) return tagStyling.isBlock(el)
      return !d.startsWith('inline') && d !== 'none' && d !== 'contents' && d !== 'ruby' && d !== 'ruby-text'
    },
    isPre(el) {
      const w = style(el)?.whiteSpace
      if (!w) return tagStyling.isPre(el)
      return w === 'pre' || w === 'pre-wrap' || w === 'pre-line' || w === 'break-spaces'
    },
  }
}

const WS = /^[ \t\r\n]*$/

const isText = (n: Node): n is Text => n.nodeType === 3 || n.nodeType === 4

/** The children crengine has: elements, and the text nodes it kept (in
 * order). Whitespace-only text (space, tab, CR, LF only — crengine's
 * IsEmptySpace) is dropped outside pre when
 * - it is the first child of a block element (ldomElementWriter::onText), or
 * - its parent has block children: autoboxChildren removes whitespace runs
 *   between blocks and trims them off the ends of each inline run. */
export function crengineChildren(parent: Element, st: Styling): Node[] {
  const raw = Array.from(parent.childNodes).filter((n) => n.nodeType === 1 || isText(n))
  if (st.isPre(parent)) return raw
  const blank = (n: Node) => isText(n) && WS.test(n.data)
  const out: Node[] = []
  // first child of a block
  let start = 0
  if (st.isBlock(parent) && raw.length && blank(raw[0])) start = 1
  const kept = raw.slice(start)
  const hasBlockChild = kept.some((n) => n.nodeType === 1 && st.isBlock(n as Element))
  if (!hasBlockChild) return kept
  // runs between block children
  let run: Node[] = []
  const flush = () => {
    let a = 0
    let b = run.length
    while (a < b && blank(run[a])) a++
    while (b > a && blank(run[b - 1])) b--
    out.push(...run.slice(a, b))
    run = []
  }
  for (const n of kept) {
    if (n.nodeType === 1 && st.isBlock(n as Element)) {
      flush()
      out.push(n)
    } else run.push(n)
  }
  flush()
  return out
}

/** The text of a node as crengine stores it, as code points, with the DOM
 * (UTF-16) index each code point starts at; `end` is the DOM length. */
function crChars(node: Text, pre: boolean): { at: number[]; end: number } {
  const s = node.data
  const at: number[] = []
  let i = 0
  // <pre>'s leading newline right after the start tag (HTML parsing rule,
  // DOM 20210904): only the element's first child
  if (pre && node.parentElement?.localName.toLowerCase() === 'pre' && node.parentElement.firstChild === node && s[0] === '\n') i = 1
  let inSpace = false
  while (i < s.length) {
    const cp = s.codePointAt(i)!
    const width = cp > 0xffff ? 2 : 1
    const c = s[i]
    if (!pre && (c === ' ' || c === '\t' || c === '\r' || c === '\n')) {
      if (!inSpace) at.push(i)
      inSpace = true
    } else if (pre && c === '\r') {
      // CRLF → LF; a lone CR becomes LF
      if (s[i + 1] !== '\n') at.push(i)
    } else {
      at.push(i)
      inSpace = false
    }
    i += width
  }
  return { at, end: s.length }
}

/** crengine offset → DOM offset in the same text node (clamped). */
export function crOffsetToDom(node: Text, offset: number, st: Styling = tagStyling): number {
  const { at, end } = crChars(node, !!node.parentElement && st.isPre(node.parentElement))
  return offset < at.length ? at[offset] : end
}

/** DOM offset → crengine offset (the code points before it). */
export function domOffsetToCr(node: Text, offset: number, st: Styling = tagStyling): number {
  const { at } = crChars(node, !!node.parentElement && st.isPre(node.parentElement))
  let n = 0
  while (n < at.length && at[n] < offset) n++
  return n
}

/* ---------- resolve: XPointer → DOM point ---------- */

export interface DomPoint {
  node: Node
  offset: number
}

/** The DOM point an XPointer names in its section document, or null when
 * any step is missing (the caller falls back to the percentage). */
export function resolveXPointer(doc: Document, x: XPointer, st: Styling = tagStyling): DomPoint | null {
  let el: Element | null = doc.body ?? doc.getElementsByTagName('body')[0] ?? null
  if (!el) return null
  for (const step of x.steps) {
    const kids = crengineChildren(el, st).filter((n): n is Element => n.nodeType === 1)
    const same = kids.filter((k) => k.localName.toLowerCase() === step.name)
    const next: Element | undefined = same[step.index - 1]
    if (!next) return null
    el = next
  }
  if (x.text) {
    const texts = crengineChildren(el, st).filter(isText)
    const t = texts[x.text.index - 1]
    if (!t) return null
    return { node: t, offset: crOffsetToDom(t, x.text.offset, st) }
  }
  if (x.point !== undefined) {
    const kids = crengineChildren(el, st)
    const child = kids[x.point]
    if (child) return { node: el, offset: Array.prototype.indexOf.call(el.childNodes, child) }
    return { node: el, offset: el.childNodes.length }
  }
  return { node: el, offset: 0 }
}

/** A collapsed Range at the pointer, for foliate's goTo anchor. */
export function xpointerToRange(doc: Document, s: string, st: Styling = tagStyling): Range | null {
  const x = parseXPointer(s)
  if (!x) return null
  const p = resolveXPointer(doc, x, st)
  if (!p) return null
  const r = doc.createRange()
  r.setStart(p.node, p.offset)
  r.collapse(true)
  return r
}

/* ---------- describe: DOM point → XPointer ---------- */

/** The first text crengine keeps at or after a DOM point, inside `root`. */
function firstTextFrom(root: Element, node: Node, offset: number, st: Styling): DomPoint | null {
  // normalise an element point to the node it sits before
  let start: Node | null = node
  let off = offset
  if (node.nodeType === 1) {
    const kids = node.childNodes
    if (offset < kids.length) {
      start = kids[offset]
      off = 0
    } else {
      start = nextNode(node, root, true)
      off = 0
    }
  }
  for (let n: Node | null = start; n; n = nextNode(n, root, false)) {
    if (isText(n) && kept(n, st)) {
      const o = n === node ? off : 0
      // a point past the last visible char of this node moves on
      const { at } = crChars(n, !!n.parentElement && st.isPre(n.parentElement))
      if (at.some((i) => i >= o && !WS.test(n.data[i]))) return { node: n, offset: o }
    }
  }
  return null
}

/** Depth-first successor within root; `skipChildren` steps over node's subtree. */
function nextNode(n: Node, root: Node, skipChildren: boolean): Node | null {
  if (!skipChildren && n.firstChild) return n.firstChild
  for (let c: Node | null = n; c && c !== root; c = c.parentNode) {
    if (c.nextSibling) return c.nextSibling
  }
  return null
}

function kept(t: Text, st: Styling): boolean {
  const p = t.parentElement
  return !!p && crengineChildren(p, st).includes(t)
}

/** The XPointer for a DOM point in spine item `spine`'s document: the first
 * visible text at or after the point (what a page's top is), with its
 * element path from <body>. Null when the point is outside <body>. */
export function xpointerFromPoint(doc: Document, spine: number, node: Node, offset: number, st: Styling = tagStyling): string | null {
  const body = doc.body ?? doc.getElementsByTagName('body')[0]
  if (!body || !body.contains(node)) return null
  const hit = firstTextFrom(body, node, offset, st)
  let target: Node
  let text: { index: number; offset: number } | undefined
  if (hit) {
    const t = hit.node as Text
    const parent = t.parentElement!
    const texts = crengineChildren(parent, st).filter(isText)
    text = { index: texts.indexOf(t) + 1, offset: domOffsetToCr(t, hit.offset, st) }
    target = parent
  } else {
    target = node.nodeType === 1 ? node : (node.parentElement ?? body)
  }
  const steps: XPointer['steps'] = []
  for (let el = target as Element; el !== body; ) {
    const parent = el.parentElement
    if (!parent) return null
    const same = crengineChildren(parent, st).filter(
      (n): n is Element => n.nodeType === 1 && (n as Element).localName.toLowerCase() === el.localName.toLowerCase(),
    )
    steps.unshift({ name: el.localName.toLowerCase(), index: same.indexOf(el) + 1 })
    el = parent
  }
  return formatXPointer({ spine, steps, text })
}
