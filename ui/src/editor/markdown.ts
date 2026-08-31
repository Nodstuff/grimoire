// Markdown ↔ ProseMirror bridge for the Tiptap StarterKit schema.
// Blocks store raw markdown; the editor works on rich nodes. Parse per block
// on load (so nodes carry their blockId), serialize per node group on save.

import { Schema, Node as PMNode } from '@tiptap/pm/model'
import {
  MarkdownParser,
  MarkdownSerializer,
  MarkdownSerializerState,
} from 'prosemirror-markdown'
import MarkdownIt from 'markdown-it'

// token map for tiptap's camelCase node names (prosemirror-markdown's
// defaults target snake_case names from its own schema)
/** markdown-it emits bare inline tokens inside th/td; the schema wants a
 * paragraph there. Wrap them so tableCell content is always block-shaped. */
function wrapCellInlines(md: ReturnType<typeof MarkdownIt>) {
  md.core.ruler.push('cell_paragraphs', (state) => {
    const out: typeof state.tokens = []
    for (let i = 0; i < state.tokens.length; i++) {
      const tok = state.tokens[i]
      out.push(tok)
      if (
        (tok.type === 'th_open' || tok.type === 'td_open') &&
        state.tokens[i + 1]?.type === 'inline'
      ) {
        const pOpen = new state.Token('paragraph_open', 'p', 1)
        const pClose = new state.Token('paragraph_close', 'p', -1)
        out.push(pOpen, state.tokens[i + 1], pClose)
        i++
      }
    }
    state.tokens = out
  })
}

export function makeParser(schema: Schema): MarkdownParser {
  const md = MarkdownIt({ html: false })
  wrapCellInlines(md)
  return new MarkdownParser(schema, md, {
    blockquote: { block: 'blockquote' },
    paragraph: { block: 'paragraph' },
    list_item: { block: 'listItem' },
    bullet_list: { block: 'bulletList' },
    ordered_list: {
      block: 'orderedList',
      getAttrs: (tok) => ({ start: +(tok.attrGet('start') ?? 1) }),
    },
    heading: {
      block: 'heading',
      getAttrs: (tok) => ({ level: +tok.tag.slice(1) }),
    },
    code_block: { block: 'codeBlock', noCloseToken: true },
    fence: {
      block: 'codeBlock',
      getAttrs: (tok) => ({ language: tok.info || null }),
      noCloseToken: true,
    },
    hr: { node: 'horizontalRule' },
    hardbreak: { node: 'hardBreak' },
    em: { mark: 'italic' },
    strong: { mark: 'bold' },
    link: {
      mark: 'link',
      getAttrs: (tok) => ({ href: tok.attrGet('href'), title: tok.attrGet('title') }),
    },
    code_inline: { mark: 'code', noCloseToken: true },
    s: { mark: 'strike' },
    table: { block: 'table' },
    thead: { ignore: true },
    tbody: { ignore: true },
    tr: { block: 'tableRow' },
    th: { block: 'tableHeader' },
    td: { block: 'tableCell' },
  })
}

export function makeSerializer(): MarkdownSerializer {
  return new MarkdownSerializer(
    {
      blockquote(state: MarkdownSerializerState, node: PMNode) {
        state.wrapBlock('> ', null, node, () => state.renderContent(node))
      },
      codeBlock(state: MarkdownSerializerState, node: PMNode) {
        state.write('```' + (node.attrs.language || '') + '\n')
        state.text(node.textContent, false)
        state.ensureNewLine()
        state.write('```')
        state.closeBlock(node)
      },
      heading(state: MarkdownSerializerState, node: PMNode) {
        state.write(state.repeat('#', node.attrs.level) + ' ')
        state.renderInline(node, false)
        state.closeBlock(node)
      },
      horizontalRule(state: MarkdownSerializerState, node: PMNode) {
        state.write(node.attrs.markup || '---')
        state.closeBlock(node)
      },
      bulletList(state: MarkdownSerializerState, node: PMNode) {
        state.renderList(node, '  ', () => '- ')
      },
      orderedList(state: MarkdownSerializerState, node: PMNode) {
        const start = node.attrs.start || 1
        const maxW = String(start + node.childCount - 1).length
        const space = state.repeat(' ', maxW + 2)
        state.renderList(node, space, (i) => {
          const nStr = String(start + i)
          return state.repeat(' ', maxW - nStr.length) + nStr + '. '
        })
      },
      listItem(state: MarkdownSerializerState, node: PMNode) {
        state.renderContent(node)
      },
      paragraph(state: MarkdownSerializerState, node: PMNode) {
        state.renderInline(node)
        state.closeBlock(node)
      },
      table(state: MarkdownSerializerState, node: PMNode) {
        const rows: string[][] = []
        let headerCols = 0
        node.forEach((row) => {
          const cells: string[] = []
          row.forEach((cell) => {
            // cells hold paragraphs; take their text with pipes escaped
            let text = ''
            cell.forEach((p) => {
              if (text) text += '<br>'
              text += p.textContent
            })
            cells.push(text.replace(/\|/g, '\\|'))
            if (cell.type.name === 'tableHeader') headerCols = cells.length
          })
          rows.push(cells)
        })
        if (rows.length === 0) return
        const width = Math.max(...rows.map((r) => r.length))
        const line = (cells: string[]) =>
          '| ' + Array.from({ length: width }, (_, i) => cells[i] ?? '').join(' | ') + ' |'
        state.write(line(rows[0]) + '\n')
        state.write('|' + Array.from({ length: width }, () => ' --- |').join('') + '\n')
        for (const r of rows.slice(1)) state.write(line(r) + '\n')
        state.closeBlock(node)
        void headerCols
      },
      tableRow() {},
      tableHeader() {},
      tableCell() {},
      hardBreak(state: MarkdownSerializerState, node: PMNode, parent: PMNode, index: number) {
        for (let i = index + 1; i < parent.childCount; i++)
          if (parent.child(i).type !== node.type) {
            state.write('\\\n')
            return
          }
      },
      text(state: MarkdownSerializerState, node: PMNode) {
        state.text(node.text ?? '')
      },
    },
    {
      italic: { open: '*', close: '*', mixable: true, expelEnclosingWhitespace: true },
      bold: { open: '**', close: '**', mixable: true, expelEnclosingWhitespace: true },
      strike: { open: '~~', close: '~~', mixable: true, expelEnclosingWhitespace: true },
      link: {
        open: '[',
        close(_state: MarkdownSerializerState, mark) {
          return `](${mark.attrs.href}${mark.attrs.title ? ` "${mark.attrs.title}"` : ''})`
        },
      },
      code: { open: '`', close: '`', escape: false },
    },
  )
}

/** Serialize a run of top-level nodes back to one markdown string. */
export function nodesToMarkdown(
  serializer: MarkdownSerializer,
  schema: Schema,
  nodes: PMNode[],
): string {
  const doc = schema.topNodeType.create(null, nodes)
  return serializer.serialize(doc, { tightLists: true }).trimEnd()
}
