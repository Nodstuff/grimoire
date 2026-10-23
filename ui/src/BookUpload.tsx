// Adding EPUBs: a dropzone (drop .epub files, or click / Enter / Space to
// pick) feeding the app's one upload queue (bookUploads.ts), with a live chip
// per file (queued → uploading → done) and one result card per file as its
// batch lands. Also mountable headless (hiddenButton + inputId) so ⌘K's
// "Add EPUBs…" has an input to click — the palette then shows the Books
// view, which renders the same queue's chips and cards.

import { useRef, useState } from 'react'
import { UploadResult } from './books'
import { bookUploads, useBookUploads } from './bookUploads'

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

  return (
    <>
      {!hiddenButton && (
        <div
          className={`book-drop ${over ? 'over' : ''}`}
          role="button"
          tabIndex={0}
          aria-label="add EPUBs: drop files here, or press Enter to pick"
          onClick={pick}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault()
              pick()
            }
          }}
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
          {busy ? 'uploading… drop more to queue them' : 'drop EPUBs here, or click to pick'}
        </div>
      )}
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
      {!hiddenButton && jobs.length > 0 && (
        <div className="book-jobs">
          {jobs.map((j, i) => (
            <span key={`${j.name}-${i}`} className={`book-job ${j.state}`} title={j.name}>
              {j.state === 'uploading' ? '↑' : j.state === 'done' ? '✓' : '…'} {j.name}
            </span>
          ))}
        </div>
      )}
      {!hiddenButton && results.map((r, i) => <ResultCard key={`${r.filename}-${i}`} r={r} />)}
    </>
  )
}

function ResultCard({ r }: { r: UploadResult }) {
  const cls = r.status === 'clash' ? 'yellow' : r.status === 'error' ? 'red' : r.status === 'already_here' ? 'dim' : 'green'
  const head =
    r.status === 'added'
      ? 'added'
      : r.status === 'updated'
        ? 'updated — new file'
        : r.status === 'already_here'
          ? 'already here'
          : r.status === 'clash'
            ? 'clash'
            : 'failed'
  return (
    <div className={`card book-result ${cls}`}>
      <div className="card-head">
        <span className={`verdict v-${cls}`}>{head}</span>
        <span className="card-meta">{r.filename}</span>
      </div>
      {r.book && (
        <div className="book-result-title">
          {r.book.title}
          {r.book.author ? ` — ${r.book.author}` : ''}
        </div>
      )}
      {r.message && <div className="meta">{r.message}</div>}
    </div>
  )
}
