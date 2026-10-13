// What the editor shows of a doc's tree, and what it leaves out in place.
import type { Block, BlockNode } from '../types'
import type { HiddenBlock } from './diff'

/** Pre-order: the blocks the editor shows; comments; every block; the
 * blocks it doesn't show (comments, frontmatter, old canvases), each after
 * the visible block it follows, so a new first block goes after the
 * frontmatter; and how many canvases the doc holds. */
export function splitTree(roots: BlockNode[]): {
  blocks: Block[]
  comments: Block[]
  allBlocks: Block[]
  hidden: HiddenBlock[]
  removedCanvases: number
} {
  const blocks: Block[] = []
  const comments: Block[] = []
  const allBlocks: Block[] = []
  const hidden: HiddenBlock[] = []
  let lastVisible: string | null = null
  let removedCanvases = 0
  const hide = (b: Block) =>
    hidden.push({ id: b.id, content: b.content, parent: b.parent_id, order_key: b.order_key, after: lastVisible })
  const walk = (nodes: BlockNode[]) => {
    for (const n of nodes) {
      allBlocks.push(n.block)
      if (n.block.block_type === 'comment') {
        comments.push(n.block)
        hide(n.block)
      }
      // the canvas editor is gone; old scenes stay in the store, unrendered
      else if (n.block.block_type === 'canvas_scene') {
        removedCanvases++
        hide(n.block)
      } else if (n.block.content.startsWith('---')) hide(n.block)
      else {
        blocks.push(n.block)
        lastVisible = n.block.id
      }
      walk(n.children)
    }
  }
  walk(roots)
  return { blocks, comments, allBlocks, hidden, removedCanvases }
}
