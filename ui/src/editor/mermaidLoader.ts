// mermaid is ~2MB: fetched on the first mermaid fence (or e-reader diagram),
// not at boot. Shared by the editor's preview and the e-reader renderer.

let mermaidMod: Promise<typeof import('mermaid')['default']> | null = null

export function loadMermaid() {
  if (!mermaidMod) {
    mermaidMod = import('mermaid').then((m) => m.default)
    mermaidMod.catch(() => {
      mermaidMod = null // let the next fence retry the fetch
    })
  }
  return mermaidMod
}
