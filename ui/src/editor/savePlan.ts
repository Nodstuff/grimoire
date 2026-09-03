// Pure decision behind DocEditor's autosave: what to do once a save has
// landed. No React so vitest covers it without a DOM.

export type AfterSave = 'clean' | 'rearm' | 'save-now'

/** After a successful save:
 * - nothing typed during the request → the editor is clean;
 * - typed while mounted → dirty, and the debounce re-arms (more keystrokes
 *   may follow, the retry clock is the backstop);
 * - typed while unmounted (the user switched docs mid-save) → save again
 *   right away: no debounce or retry clock survives the unmount, and those
 *   keystrokes are not in what landed. */
export function afterSave({ editedMeanwhile, mounted }: { editedMeanwhile: boolean; mounted: boolean }): AfterSave {
  if (!editedMeanwhile) return 'clean'
  return mounted ? 'rearm' : 'save-now'
}
