// Adding EPUBs (canvas Books, the dashed dropzone): drop .epub files on it
// (or anywhere on the Books page), or "Choose files…", feeding the app's
// one upload queue (bookUploads.ts). Below it, one pill per file: queued /
// uploading while the run goes, then its result (added, updated, already
// here, clash, failed). Also mountable headless (hiddenButton + inputId) so
// ⌘K's "Add EPUBs…" and the page's Add EPUBs button have an input to click;
// the Books page renders the same queue's pills.

import { useRef, useState } from 'react'
import { UploadResult } from './books'
import { bookUploads, useBookUploads, type UploadJob } from './bookUploads'
import { Button, Icon, Spinner } from './lapis'

export default function BookUpload({
  inputId,
  hiddenButton = false,
}: {
  /** id on the file input so a palette command can trigger it */
  inputId?: string
  /** render only the input (the App-root instance behind ⌘K) */
  hiddenButton?: boolean
}) {
  const input = useRef<HTMLInputElement>(null)
  const [over, setOver] = useState(false)
  const { busy, jobs, results } = useBookUploads()

  const pick = () => input.current?.click()
  const fileInput = (
    <input
      id={inputId}
      ref={input}
      type="file"
      hidden
      multiple
      accept=".epub,application/epub+zip"
      onChange={(e) => {
        // copy before clearing: the FileList is live
        const files = e.target.files ? Array.from(e.target.files) : []
        e.target.value = ''
        bookUploads.enqueue(files)
      }}
    />
  )
  if (hiddenButton) return fileInput

  // while a run goes, the files still waiting or uploading; the rest have results
  const live = jobs.filter((j) => j.state !== 'done')

  return (
    <section
      aria-label="Upload EPUBs"
      className={`bk-drop${over ? ' over' : ''}`}
      onDragOver={(e) => {
        e.preventDefault()
        setOver(true)
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        // the page-level guard (Books) would take it too; one enqueue
        e.preventDefault()
        e.stopPropagation()
        setOver(false)
        bookUploads.enqueue(e.dataTransfer.files)
      }}
    >
      <div className="bk-drop-row">
        <span className="bk-drop-icon" aria-hidden>
          <Icon name="upload" />
        </span>
        <span className="bk-drop-text">
          <b>{busy ? 'Uploading… drop more to queue them' : 'Drop EPUBs anywhere on this page'}</b>
          <span className="muted">They appear on your e-readers at their next sync.</span>
        </span>
        <Button size="sm" onClick={pick}>
          Choose files…
        </Button>
      </div>
      {fileInput}
      {(live.length > 0 || results.length > 0) && (
        <ul className="bk-results" aria-live="polite">
          {results.map((r, i) => (
            <ResultPill key={`r-${r.filename}-${i}`} r={r} />
          ))}
          {live.map((j, i) => (
            <JobPill key={`j-${j.name}-${i}`} j={j} />
          ))}
        </ul>
      )}
    </section>
  )
}

function JobPill({ j }: { j: UploadJob }) {
  return (
    <li className="bk-res">
      {j.state === 'uploading' ? <Spinner size={14} /> : <Icon name="clock" size={15} style={{ color: 'var(--ink3)' }} />}
      <b>{j.name}</b>
      {j.state === 'uploading' ? 'uploading' : 'queued'}
    </li>
  )
}

/** One result: icon + file + what happened, coloured by outcome (never by
 * colour alone: the word says it). */
export function resultLine(r: UploadResult): { tone: 'green' | 'dim' | 'yellow' | 'red'; text: string } {
  switch (r.status) {
    case 'added':
      return { tone: 'green', text: 'added' }
    case 'updated':
      return { tone: 'green', text: 'updated · new file' }
    case 'already_here':
      return { tone: 'dim', text: 'already here' }
    case 'clash':
      return { tone: 'yellow', text: r.message ? `clash · ${r.message}` : 'clashes with a book already here' }
    default:
      return { tone: 'red', text: r.message ? `failed · ${r.message}` : 'failed' }
  }
}

function ResultPill({ r }: { r: UploadResult }) {
  const { tone, text } = resultLine(r)
  const icon = tone === 'green' ? 'applied' : tone === 'yellow' ? 'flagged' : tone === 'red' ? 'error' : null
  return (
    <li className={`bk-res bk-res-${tone}`} title={r.book ? `${r.book.title}${r.book.author ? ` · ${r.book.author}` : ''}` : undefined}>
      {icon ? (
        <Icon name={icon} size={15} />
      ) : (
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" aria-hidden focusable="false">
          <circle cx="12" cy="12" r="8.5" />
          <path d="M8 12h8" />
        </svg>
      )}
      <b>{r.filename}</b>
      <span>{text}</span>
    </li>
  )
}
