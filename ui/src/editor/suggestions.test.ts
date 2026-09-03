import { describe, expect, it } from 'vitest'
import { EditorState } from '@tiptap/pm/state'
import { schema } from './DocEditor'
import { acceptSuggestion, collectSuggestions, pendingSuggestionIds, rejectSuggestion } from './Suggestions'

const p = (text: string, attrs: Record<string, unknown> = {}) =>
  schema.nodes.paragraph.create(attrs, text ? schema.text(text) : undefined)

function state(nodes: ReturnType<typeof p>[]) {
  return EditorState.create({ doc: schema.nodes.doc.create(null, nodes) })
}
const texts = (s: EditorState) => {
  const out: string[] = []
  s.doc.forEach((n) => out.push(n.textContent))
  return out
}

describe('suggestions', () => {
  const S = { suggestionId: 's1', suggestionBy: 'scribe' }
  const doc = () =>
    state([
      p('alpha'),
      p('inserted', { suggestion: 'insert', ...S }),
      p('beta', { suggestion: 'replaced', suggestionId: 's2', suggestionBy: 'scribe' }),
      p('BETA', { suggestion: 'replace', suggestionId: 's2', suggestionBy: 'scribe' }),
      p('gamma'),
    ])

  it('collects groups and pending ids in order', () => {
    const all = collectSuggestions(doc().doc)
    expect(all.map((s) => `${s.kind}:${s.id}`)).toEqual(['insert:s1', 'replaced:s2', 'replace:s2'])
    expect(pendingSuggestionIds(doc().doc)).toEqual(['s1', 's2'])
  })

  it('accept insert keeps the text and clears the mark', () => {
    const s = doc()
    const next = s.apply(acceptSuggestion(s, 's1')!)
    expect(texts(next)).toEqual(['alpha', 'inserted', 'beta', 'BETA', 'gamma'])
    expect(pendingSuggestionIds(next.doc)).toEqual(['s2'])
  })

  it('accept replace swaps the original for the new text', () => {
    const s = doc()
    const next = s.apply(acceptSuggestion(s, 's2')!)
    expect(texts(next)).toEqual(['alpha', 'inserted', 'BETA', 'gamma'])
    expect(collectSuggestions(next.doc).map((x) => x.id)).toEqual(['s1'])
  })

  it('reject replace keeps the original and drops the new text', () => {
    const s = doc()
    const next = s.apply(rejectSuggestion(s, 's2')!)
    expect(texts(next)).toEqual(['alpha', 'inserted', 'beta', 'gamma'])
    expect(collectSuggestions(next.doc).map((x) => x.kind)).toEqual(['insert'])
  })

  it('reject insert removes it', () => {
    const s = doc()
    const next = s.apply(rejectSuggestion(s, 's1')!)
    expect(texts(next)).toEqual(['alpha', 'beta', 'BETA', 'gamma'])
  })

  it('unknown id is a no-op', () => {
    expect(acceptSuggestion(doc(), 'nope')).toBeNull()
  })
})
