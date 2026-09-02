// "I have a folder of markdown": the browser cannot hand the daemon a path,
// so we read the chosen folder's .md files here and POST {path, content}
// pairs; the daemon runs the same import as `grimoire import`. Folders become
// docs with children, files become docs — one round-trip, one notice.

import { useRef, useState } from 'react'
import { api } from './types'
import { notify, errText } from './Notice'

export interface ImportResult {
  docs: number
  blocks: number
  skipped: string[]
}

const MD = /\.(md|markdown)$/i

/** Read every markdown file from a directory picker's FileList. Paths keep
 * their folder structure (`webkitRelativePath`) minus the chosen root. */
export async function collectMarkdown(files: FileList | File[]): Promise<{ path: string; content: string }[]> {
  const out: { path: string; content: string }[] = []
  for (const f of Array.from(files)) {
    if (!MD.test(f.name)) continue
    const rel = (f as File & { webkitRelativePath?: string }).webkitRelativePath || f.name
    // drop the root folder segment so the import lands at the top level
    const parts = rel.split('/')
    const path = parts.length > 1 ? parts.slice(1).join('/') : rel
    if (path.split('/').some((seg) => seg.startsWith('.'))) continue
    out.push({ path, content: await f.text() })
  }
  return out
}

export function importSummary(r: ImportResult): string {
  const skipped = r.skipped.length ? ` · ${r.skipped.length} skipped` : ''
  return `imported ${r.docs} doc${r.docs === 1 ? '' : 's'} (${r.blocks} blocks)${skipped}`
}

/** A button that opens the OS folder picker and imports. `label` is the
 * visible text; the input itself is hidden (WKWebView renders it as a
 * native chooser either way). */
export default function ImportFolder({
  label = 'Import a folder of Markdown…',
  className = 'chip',
  onDone,
  inputId,
  hiddenButton = false,
}: {
  label?: string
  className?: string
  onDone?: (r: ImportResult) => void
  /** id on the file input so a palette command can trigger it */
  inputId?: string
  /** render only the input (for the App-root instance behind ⌘K) */
  hiddenButton?: boolean
}) {
  const input = useRef<HTMLInputElement>(null)
  const [busy, setBusy] = useState(false)
  const run = async (files: FileList | null) => {
    if (!files || files.length === 0) return
    setBusy(true)
    try {
      const md = await collectMarkdown(files)
      if (md.length === 0) {
        notify('no .md files in that folder', 'warn')
        return
      }
      const r = await api<ImportResult>('/api/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ files: md }),
      })
      notify(importSummary(r), 'ok', { ttlMs: 8000 })
      onDone?.(r)
    } catch (e) {
      notify(`import failed: ${errText(e)}`)
    } finally {
      setBusy(false)
      if (input.current) input.current.value = ''
    }
  }
  return (
    <>
      {!hiddenButton && (
        <button className={className} disabled={busy} onClick={() => input.current?.click()}>
          {busy ? 'importing…' : label}
        </button>
      )}
      <input
        id={inputId}
        ref={input}
        type="file"
        hidden
        multiple
        // @ts-expect-error non-standard but universal on desktop WebKit/Chromium
        webkitdirectory=""
        onChange={(e) => run(e.target.files)}
      />
    </>
  )
}
