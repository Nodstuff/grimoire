import { describe, expect, it } from 'vitest'
import { collectMarkdown, importSummary } from './ImportFolder'

function file(rel: string, text: string): File {
  const f = new File([text], rel.split('/').pop()!, { type: 'text/markdown' })
  Object.defineProperty(f, 'webkitRelativePath', { value: rel })
  return f
}

describe('collectMarkdown', () => {
  it('keeps only markdown, strips the chosen root, skips dotfiles', async () => {
    const out = await collectMarkdown([
      file('Vault/a.md', '# A'),
      file('Vault/sub/b.markdown', 'b'),
      file('Vault/.obsidian/x.md', 'no'),
      file('Vault/img.png', 'bin'),
    ])
    expect(out.map((f) => f.path)).toEqual(['a.md', 'sub/b.markdown'])
    expect(out[0].content).toBe('# A')
  })
  it('a bare file with no folder keeps its name', async () => {
    const out = await collectMarkdown([new File(['x'], 'note.md')])
    expect(out[0].path).toBe('note.md')
  })
})

describe('importSummary', () => {
  it('reads naturally', () => {
    expect(importSummary({ docs: 1, blocks: 4, skipped: [] })).toBe('imported 1 doc (4 blocks)')
    expect(importSummary({ docs: 12, blocks: 300, skipped: ['a', 'b'] })).toBe('imported 12 docs (300 blocks) · 2 skipped')
  })
})
