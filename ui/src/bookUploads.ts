// The one EPUB upload queue the whole app shares: the Books dropzone, a drop
// anywhere on the Books page and ⌘K's "Add EPUBs…" all feed it, and the Books
// view renders its chips and result cards — so a run started from the palette
// shows the same progress as one started on the page. A second drop while a
// run is going is queued behind it (and says so), never dropped. Uploads go
// in byte-sized batches (byteBatches), one POST at a time.

import { useSyncExternalStore } from 'react'
import { errText, notify as defaultNotify, type NoticeKind, type NotifyOpts } from './Notice'
import { UploadResult, byteBatches, collectEpubs, uploadBooks, uploadResultSummary } from './books'

export interface UploadJob {
  name: string
  state: 'queued' | 'uploading' | 'done'
}

export interface UploadState {
  busy: boolean
  jobs: UploadJob[]
  results: UploadResult[]
  /** bumps once per finished run (the Books view reloads on it) */
  runs: number
}

type Notify = (text: string, kind?: NoticeKind, opts?: NotifyOpts) => void

export interface UploadQueue {
  get: () => UploadState
  subscribe: (fn: () => void) => () => void
  /** files from a pick or drop; non-EPUBs are skipped and counted */
  enqueue: (files: FileList | File[] | null | undefined) => void
  /** resolves when nothing is uploading or queued (tests) */
  idle: () => Promise<void>
}

export function createUploadQueue(
  upload: (files: File[]) => Promise<UploadResult[]> = uploadBooks,
  notify: Notify = defaultNotify,
): UploadQueue {
  let state: UploadState = { busy: false, jobs: [], results: [], runs: 0 }
  const listeners = new Set<() => void>()
  const set = (patch: Partial<UploadState>) => {
    state = { ...state, ...patch }
    listeners.forEach((fn) => fn())
  }
  // files waiting for the current run, with their job index and how many
  // non-EPUBs their drop skipped
  let pending: { file: File; job: number }[] = []
  let pendingSkipped = 0
  let running: Promise<void> | null = null

  const mark = (idx: Set<number>, s: UploadJob['state']) =>
    set({ jobs: state.jobs.map((j, i): UploadJob => (idx.has(i) ? { ...j, state: s } : j)) })

  const drain = async () => {
    while (pending.length > 0) {
      const run = pending
      const skipped = pendingSkipped
      pending = []
      pendingSkipped = 0
      const all: UploadResult[] = []
      for (const batch of byteBatches(run, (x) => x.file.size)) {
        const idx = new Set(batch.map((x) => x.job))
        mark(idx, 'uploading')
        let rs: UploadResult[]
        try {
          rs = await upload(batch.map((x) => x.file))
        } catch (e) {
          // the whole POST failed: every file in the batch gets the reason
          rs = batch.map((x): UploadResult => ({ filename: x.file.name, status: 'error', message: errText(e) }))
        }
        all.push(...rs)
        mark(idx, 'done')
        set({ results: [...state.results, ...rs] })
      }
      const problems = all.some((r) => r.status === 'clash' || r.status === 'error')
      notify(uploadResultSummary(all, skipped), problems ? 'warn' : 'ok', { ttlMs: 10_000 })
      set({ runs: state.runs + 1 })
    }
    set({ busy: false })
    running = null
  }

  const enqueue = (files: FileList | File[] | null | undefined) => {
    if (!files) return
    const list = Array.from(files)
    if (list.length === 0) return
    const epubs = collectEpubs(list)
    const skipped = list.length - epubs.length
    if (epubs.length === 0) {
      notify(skipped === 1 ? 'that is not an .epub file — nothing uploaded' : `none of those ${skipped} files is an .epub — nothing uploaded`, 'warn')
      return
    }
    const fresh = !state.busy
    const base = fresh ? 0 : state.jobs.length
    const jobs: UploadJob[] = epubs.map((f) => ({ name: f.name, state: 'queued' }))
    pending.push(...epubs.map((file, i) => ({ file, job: base + i })))
    pendingSkipped += skipped
    if (fresh) {
      set({ busy: true, jobs, results: [] })
      running = drain()
    } else {
      set({ jobs: [...state.jobs, ...jobs] })
      notify(`queued ${epubs.length} EPUB${epubs.length === 1 ? '' : 's'} — they upload after the current run`, 'ok')
    }
  }

  return {
    get: () => state,
    subscribe: (fn) => {
      listeners.add(fn)
      return () => listeners.delete(fn)
    },
    enqueue,
    idle: async () => {
      while (running) await running
    },
  }
}

/** The app's queue. */
export const bookUploads = createUploadQueue()

export function useBookUploads(q: UploadQueue = bookUploads): UploadState {
  return useSyncExternalStore(q.subscribe, q.get, q.get)
}
