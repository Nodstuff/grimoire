// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import {
  crOffsetToDom,
  domOffsetToCr,
  formatXPointer,
  parseXPointer,
  resolveXPointer,
  xpointerFromPoint,
  xpointerToRange,
} from './xpointer'

const xhtml = (body: string) =>
  new DOMParser().parseFromString(
    `<?xml version="1.0" encoding="utf-8"?>\n<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title></head><body>${body}</body></html>`,
    'application/xhtml+xml',
  )

/** The text from a resolved point to the end of its node. */
function at(doc: Document, xp: string): string | null {
  const x = parseXPointer(xp)
  if (!x) return null
  const p = resolveXPointer(doc, x)
  if (!p) return null
  return p.node.nodeType === 3 ? (p.node as Text).data.slice(p.offset) : (p.node as Element).localName
}

describe('parseXPointer', () => {
  it('reads the forms KOReader writes', () => {
    expect(parseXPointer('/body/DocFragment[23]/body/p[1]/text()[1].0')).toEqual({
      spine: 22,
      steps: [{ name: 'p', index: 1 }],
      text: { index: 1, offset: 0 },
    })
    expect(parseXPointer('/body/DocFragment[12]/body/p[6]/text()[2].297')).toEqual({
      spine: 11,
      steps: [{ name: 'p', index: 6 }],
      text: { index: 2, offset: 297 },
    })
    // a bare step is the first of its name; a bare text() the first text
    expect(parseXPointer('/body/DocFragment[8]/body/div/p[9]/text().54')).toEqual({
      spine: 7,
      steps: [
        { name: 'div', index: 1 },
        { name: 'p', index: 9 },
      ],
      text: { index: 1, offset: 54 },
    })
  })

  it('reads element pointers, a bare DocFragment and body[1] (DOM 20260812)', () => {
    expect(parseXPointer('/body/DocFragment[3]/body')).toEqual({ spine: 2, steps: [] })
    expect(parseXPointer('/body/DocFragment/body/div[2]')).toEqual({ spine: 0, steps: [{ name: 'div', index: 2 }] })
    expect(parseXPointer('/body/DocFragment[4]/body[1]/section[1]/p[1].0')).toEqual({
      spine: 3,
      steps: [
        { name: 'section', index: 1 },
        { name: 'p', index: 1 },
      ],
      point: 0,
    })
  })

  it('refuses what is not an EPUB body pointer', () => {
    for (const s of ['', '42', '/body/p[1]', '/body/DocFragment[0]/body', '/body/DocFragment[2]/body/text()[1]/p[1]', '/body/DocFragment[2]/body/p[x]']) {
      expect(parseXPointer(s)).toBeNull()
    }
  })

  it('writes the canonical form: explicit indexes, bare body', () => {
    const x = parseXPointer('/body/DocFragment[8]/body/div/p[9]/text().54')!
    expect(formatXPointer(x)).toBe('/body/DocFragment[8]/body/div[1]/p[9]/text()[1].54')
  })
})

describe('resolving into a section document', () => {
  it('p[1]/text()[1].0 and a second text node after an inline', () => {
    const doc = xhtml('\n  <p>First paragraph.</p>\n  <p>Second <i>one</i> here and there</p>\n')
    expect(at(doc, '/body/DocFragment[23]/body/p[1]/text()[1].0')).toBe('First paragraph.')
    expect(at(doc, '/body/DocFragment[23]/body/p[2]/text()[1].3')).toBe('ond ')
    expect(at(doc, '/body/DocFragment[23]/body/p[2]/text()[2].3')).toBe('re and there')
    expect(at(doc, '/body/DocFragment[23]/body/p[3]/text()[1].0')).toBeNull()
  })

  it('div/p[9]/text().54 counts same-named siblings only', () => {
    const ps = Array.from({ length: 10 }, (_, i) => `<p>Paragraph ${i + 1}: ${'word '.repeat(20)}end.</p>`).join('\n')
    const doc = xhtml(`<div>\n<h2>Chapter</h2>\n${ps}\n</div>`)
    const text = `Paragraph 9: ${'word '.repeat(20)}end.`
    expect(at(doc, '/body/DocFragment[8]/body/div/p[9]/text().54')).toBe(text.slice(54))
  })

  it('collapses whitespace runs like crengine: offsets count the collapsed text', () => {
    const doc = xhtml('<p>Hello,\n      world and more</p>')
    // crengine has "Hello, world and more": offset 7 is the w
    expect(at(doc, '/body/DocFragment[1]/body/p[1]/text()[1].7')).toBe('world and more')
    const t = doc.getElementsByTagName('p')[0].firstChild as Text
    expect(crOffsetToDom(t, 7)).toBe(13)
    expect(domOffsetToCr(t, 13)).toBe(7)
    // inside the run, the DOM point is past the one space crengine kept
    expect(domOffsetToCr(t, 9)).toBe(7)
  })

  it('drops the whitespace a block starts with, so text()[1] is the tail', () => {
    const doc = xhtml('<p>\n  <span>a</span> tail end</p>')
    expect(at(doc, '/body/DocFragment[1]/body/p[1]/text()[1].1')).toBe('tail end')
    // and keeps whitespace between inlines: it is a text node there
    const doc2 = xhtml('<p><i>a</i> <b>b</b> c</p>')
    expect(at(doc2, '/body/DocFragment[1]/body/p[1]/text()[2].1')).toBe('c')
  })

  it('whitespace between blocks is not a text node', () => {
    const doc = xhtml('<div>\n<p>a</p>\n  <p>b</p>\n</div>')
    expect(at(doc, '/body/DocFragment[1]/body/div[1]/text()[1].0')).toBeNull()
    // but text in a mixed container is, trimmed of its blank ends
    const doc2 = xhtml('<div>\n<p>a</p>\n loose text \n<p>b</p></div>')
    // (its leading "\n " is one space in crengine: offset 1 is the l)
    expect(at(doc2, '/body/DocFragment[1]/body/div[1]/text()[1].1')).toBe('loose text \n')
  })

  it('skips comments and counts code points, not UTF-16 units', () => {
    const doc = xhtml('<p>one<!-- note -->two 😀 smile</p>')
    // the comment splits the text: two text nodes, as in crengine
    expect(at(doc, '/body/DocFragment[1]/body/p[1]/text()[2].0')).toBe('two 😀 smile')
    // "two 😀 " is 6 code points (7 UTF-16 units)
    expect(at(doc, '/body/DocFragment[1]/body/p[1]/text()[2].6')).toBe('smile')
  })

  it('keeps pre text as is, minus the leading newline', () => {
    const doc = xhtml('<pre>\nline one\n  line two</pre>')
    expect(at(doc, '/body/DocFragment[1]/body/pre[1]/text()[1].0')).toBe('line one\n  line two')
    expect(at(doc, '/body/DocFragment[1]/body/pre[1]/text()[1].11')).toBe('line two')
  })

  it('an element pointer lands on the element; a range collapses there', () => {
    const doc = xhtml('<div><p>a</p></div><div><p>b</p></div>')
    expect(at(doc, '/body/DocFragment[12]/body/div[2]')).toBe('div')
    const r = xpointerToRange(doc, '/body/DocFragment[12]/body/div[2]/p/text().0')!
    expect(r.collapsed).toBe(true)
    expect((r.startContainer as Text).data).toBe('b')
  })
})

describe('describing a DOM point', () => {
  it('round-trips with resolve, in the canonical form', () => {
    const doc = xhtml('\n<div>\n<p>Intro text.</p>\n<p>Second <em>em</em>   tail   words</p>\n</div>\n')
    const p2 = doc.getElementsByTagName('p')[1]
    const tail = p2.lastChild as Text // "   tail   words"
    const xp = xpointerFromPoint(doc, 7, tail, tail.data.indexOf('words'))!
    expect(xp).toBe('/body/DocFragment[8]/body/div[1]/p[2]/text()[2].6')
    expect(at(doc, xp)).toBe('words')
  })

  it('an element point moves to the first visible text after it', () => {
    const doc = xhtml('<section>\n  <h1>Title</h1>\n  <p>Body</p>\n</section>')
    const section = doc.getElementsByTagName('section')[0]
    // the point before <h1>
    expect(xpointerFromPoint(doc, 0, section, 0)).toBe('/body/DocFragment[1]/body/section[1]/h1[1]/text()[1].0')
    // the point between h1 and p
    const p = doc.getElementsByTagName('p')[0]
    const idx = Array.prototype.indexOf.call(section.childNodes, p)
    expect(xpointerFromPoint(doc, 0, section, idx)).toBe('/body/DocFragment[1]/body/section[1]/p[1]/text()[1].0')
  })

  it('a point at the end of a text moves to the next visible text', () => {
    const doc = xhtml('<p>alpha</p><p>beta</p>')
    const t = doc.getElementsByTagName('p')[0].firstChild as Text
    expect(xpointerFromPoint(doc, 2, t, 5)).toBe('/body/DocFragment[3]/body/p[2]/text()[1].0')
  })

  it('writes what KOReader would read back to the same text', () => {
    // KOReader's own pointer and ours name the same character
    const doc = xhtml('<div>\n<p>x</p>\n<p>y</p>\n<p>The quick brown fox</p></div>')
    const kosync = '/body/DocFragment[8]/body/div/p[3]/text().4'
    const r = xpointerToRange(doc, kosync)!
    const ours = xpointerFromPoint(doc, 7, r.startContainer, r.startOffset)!
    expect(ours).toBe('/body/DocFragment[8]/body/div[1]/p[3]/text()[1].4')
    expect(at(doc, ours)).toBe('quick brown fox')
  })
})
