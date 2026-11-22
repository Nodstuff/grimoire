// The mounted doc editors, by doc id, for the code around them that must
// not act over unsaved typing: a reload, Keep / Revert, a share snapshot,
// a refresh from the server. No React: a module-level map the editor
// registers itself in while mounted.

import type { Block } from '../types'

export interface EditorHandle {
  /** Land whatever is unsaved now (the debounce skipped). True when nothing
   * is left unsaved; false when a save failed or typing went on. */
  flush(): Promise<boolean>
  /** When the editor is clean and already shows exactly `blocks` (the
   * visible blocks of a fresher tree), adopt `epoch` as its base and say
   * so: no remount needed. False otherwise. */
  catchUp(blocks: Block[], epoch: number): boolean
}

const handles = new Map<string, EditorHandle>()

/** Register while mounted; the returned function unregisters (only if this
 * handle is still the doc's: a remount registers the new one first). */
export function registerEditor(docId: string, h: EditorHandle): () => void {
  handles.set(docId, h)
  return () => {
    if (handles.get(docId) === h) handles.delete(docId)
  }
}

export function editorFor(docId: string): EditorHandle | null {
  return handles.get(docId) ?? null
}

/** Flush the doc's editor, if one is mounted. True when nothing is unsaved. */
export async function flushDoc(docId: string): Promise<boolean> {
  const h = handles.get(docId)
  if (!h) return true
  try {
    return await h.flush()
  } catch {
    return false
  }
}

/** Flush every mounted editor. True when nothing is unsaved anywhere. */
export async function flushAll(): Promise<boolean> {
  const all = await Promise.all([...handles.keys()].map(flushDoc))
  return all.every(Boolean)
}
