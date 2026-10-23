import { describe, expect, it } from 'vitest'
import { tokenize } from './CodeHighlight'

const pick = (code: string, lang: string) => tokenize(code, lang).map((t) => [code.slice(t.from, t.to), t.cls])

describe('tokenize', () => {
  it('colours keywords, types, strings, numbers and calls', () => {
    expect(pick('pub fn score(op: &Op) -> Scored { red(1.0, "x") }', 'rust')).toEqual([
      ['pub', 'k1'],
      ['fn', 'k1'],
      ['score', 'k4'],
      ['Op', 'k4'],
      ['Scored', 'k4'],
      ['red', 'k4'],
      ['1.0', 'k3'],
      ['"x"', 'k2'],
    ])
  })
  it('knows which languages comment with #', () => {
    expect(pick('x = 1 # note', 'python')).toContainEqual(['# note', 'kc'])
    expect(pick('#[derive(Debug)]', 'rust').some(([, c]) => c === 'kc')).toBe(false)
    expect(pick('// note', 'ts')).toEqual([['// note', 'kc']])
  })
})
