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
export function makeParser(schema: Schema): MarkdownParser {
  return new MarkdownParser(schema, MarkdownIt('commonmark', { html: false }), {
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
