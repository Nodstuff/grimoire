// Light syntax colour for code blocks (Lapis Doc artboard: keywords violet,
// strings green, numbers amber, types lapis, comments dim). A small regex
// tokenizer painted as inline decorations: the text stays plain, so the
// diff, the markdown and the caret never see it. Diagram fences are skipped.

import { Extension } from '@tiptap/core'
import { Plugin, PluginKey, type EditorState } from '@tiptap/pm/state'
import { Decoration, DecorationSet } from '@tiptap/pm/view'
import { DIAGRAM_LANGS } from './diagramLangs'

const KEYWORDS = new Set(
  (
    'as async await break case catch class const continue crate def default defer do dyn elif else enum export extends ' +
    'false fn for from func function go if impl import in interface let loop match mod move mut new nil null ' +
    'package pass pub raise ref return self Self static struct super switch then throw trait true try type ' +
    'typeof unsafe use var void where while with yield None True False'
  ).split(' '),
)

export type Tok = { from: number; to: number; cls: 'k1' | 'k2' | 'k3' | 'k4' | 'kc' }

const RE =
  /(\/\/[^\n]*|#(?![\[!])[^\n]*|\/\*[\s\S]*?\*\/)|("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`)|\b(\d[\d_]*(?:\.\d+)?)\b|\b([A-Za-z_][A-Za-z0-9_]*)\b/g

/** Tokens to colour in `code` for `lang` (only the ones worth a colour). */
export function tokenize(code: string, lang: string): Tok[] {
  const hashComments = /^(py|python|sh|bash|zsh|shell|yaml|yml|toml|rb|ruby|r|make|dockerfile|conf|ini)$/i.test(lang)
  const out: Tok[] = []
  RE.lastIndex = 0
  let m: RegExpExecArray | null
  while ((m = RE.exec(code))) {
    const [all, comment, str, num, word] = m
    const from = m.index
    const to = from + all.length
    if (comment) {
      if (comment.startsWith('#') && !hashComments) {
        // not a comment in this language: rescan after the '#'
        RE.lastIndex = from + 1
        continue
      }
      out.push({ from, to, cls: 'kc' })
    } else if (str) out.push({ from, to, cls: 'k2' })
    else if (num) out.push({ from, to, cls: 'k3' })
    else if (word) {
      if (KEYWORDS.has(word)) out.push({ from, to, cls: 'k1' })
      else if (/^[A-Z][a-z0-9]/.test(word) || code[to] === '(') out.push({ from, to, cls: 'k4' })
    }
  }
  return out
}

function build(state: EditorState): DecorationSet {
  const decos: Decoration[] = []
  state.doc.descendants((node, pos) => {
    if (node.type.name !== 'codeBlock') return true
    const lang = String(node.attrs.language ?? '').toLowerCase()
    if (!lang || DIAGRAM_LANGS.has(lang)) return false
    const start = pos + 1
    for (const t of tokenize(node.textContent, lang))
      decos.push(Decoration.inline(start + t.from, start + t.to, { class: `tok-${t.cls}` }))
    return false
  })
  return DecorationSet.create(state.doc, decos)
}

export const CodeHighlight = Extension.create({
  name: 'codeHighlight',
  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: new PluginKey('codeHighlight'),
        state: {
          init: (_, state) => build(state),
          apply: (tr, old, _o, next) => (tr.docChanged ? build(next) : old),
        },
        props: {
          decorations(state) {
            return this.getState(state)
          },
        },
      }),
    ]
  },
})
