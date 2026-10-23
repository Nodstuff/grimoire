// Adding EPUBs: a dropzone (drop .epub files or click to pick) that uploads
// in batches of UPLOAD_CHUNK per POST, with a live chip per file
// (queued → uploading → done) and one result card per file as its batch
// lands. One summary toast rolls the whole run up. Also mountable headless
// (hiddenButton + inputId) so ⌘K's "Add EPUBs…" has an input to click —
// the same trick as ImportFolder's "import-folder-input".

import { useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { UploadResult, chunks, collectEpubs, uploadBooks, uploadResultSummary } from './books'

interface Job {
  name: string
  state: 'queued' | 'uploading' | 'done'
}

export default function BookUpload({
  onUploaded,
  inputId,
  hiddenButton = false,
}: {
  /** called once per run with every batch's results (for the parent to reload) */
  onUploaded?: (results: UploadResult[]) => void
  /** id on the file input so a palette command can trigger it */
  inputId?: string
  /** render only the input (the App-root instance behind ⌘K) */
  hiddenButton?: boolean
}) {
  const input = useRef<HTMLInputElement>(null)
  const [over, setOver] = useState(false)
  const [jobs, setJobs] = useState<Job[]>([])
  const [results, setResults] = useState<UploadResult[]>([])
  const [busy, setBusy] = useState(false)

  const run = async (picked: FileList | File[] | null) => {
    if (!picked || busy) return
    const epubs = collectEpubs(Array.from(picked))
    if (input.current) input.current.value = ''
    if (epubs.length === 0) {
      notify('no .epub files there', 'warn')
      return
    }
    setBusy(true)
    setResults([])
    setJobs(epubs.map((f) => ({ name: f.name, state: 'queued' })))
    const all: UploadResult[] = []
    // sequential batches: a big import should not hammer the daemon with
    // parallel multi-MB POSTs
    for (const batch of chunks(epubs.map((f, i) => ({ f, i })))) {
      const idx = new Set(batch.map((x) => x.i))
      setJobs((cur) => cur.map((j, i): Job => (idx.has(i) ? { ...j, state: 'uploading' } : j)))
      let rs: UploadResult[]
      try {
        rs = await uploadBooks(batch.map((x) => x.f))
      } catch (e) {
        // the whole POST failed: every file in the batch gets the reason
        rs = batch.map((x): UploadResult => ({ filename: x.f.name, status: 'error', message: errText(e) }))
      }
      all.push(...rs)
      setJobs((cur) => cur.map((j, i): Job => (idx.has(i) ? { ...j, state: 'done' } : j)))
      setResults((cur) => [...cur, ...rs])
    }
    const problems = all.some((r) => r.status === 'clash' || r.status === 'error')
    notify(uploadResultSummary(all), problems ? 'warn' : 'ok', { ttlMs: 10_000 })
    onUploaded?.(all)
    setBusy(false)
  }

  return (
    <>
      {!hiddenButton && (
        <div
          className={`book-drop ${over ? 'over' : ''}`}
          role="button"
          tabIndex={0}
          onClick={() => !busy && input.current?.click()}
          onKeyDown={(e) => e.key === 'Enter' && !busy && input.current?.click()}
          onDragOver={(e) => {
            e.preventDefault()
            setOver(true)
          }}
          onDragLeave={() => setOver(false)}
          onDrop={(e) => {
            e.preventDefault()
            setOver(false)
            run(e.dataTransfer.files)
          }}
        >
          {busy ? 'uploading…' : 'drop EPUBs here, or click to pick'}
        </div>
      )}
      <input
        id={inputId}
        ref={input}
        type="file"
        hidden
        multiple
        accept=".epub,application/epub+zip"
        onChange={(e) => run(e.target.files)}
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
