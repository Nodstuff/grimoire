import { describe, expect, it } from 'vitest'
import { createUploadQueue } from './bookUploads'
import type { UploadResult } from './books'

const MB = 1024 * 1024

function file(name: string, bytes = 1): File {
  return new File([new Uint8Array(bytes)], name)
}

/** An upload whose POSTs resolve only when the test says so. */
function gatedUpload() {
  const posts: { names: string[]; release: () => void }[] = []
  const upload = (files: File[]) =>
    new Promise<UploadResult[]>((resolve) => {
      const names = files.map((f) => f.name)
      posts.push({ names, release: () => resolve(names.map((filename) => ({ filename, status: 'added' }))) })
    })
  return { posts, upload }
}

const tick = () => new Promise((r) => setTimeout(r, 0))

describe('the upload queue', () => {
  it('uploads EPUBs in byte batches and toasts once per run, counting skipped files', async () => {
    const sent: string[][] = []
    const toasts: [string, string | undefined][] = []
    const q = createUploadQueue(
      async (files) => {
        sent.push(files.map((f) => f.name))
        return files.map((f): UploadResult => ({ filename: f.name, status: 'added' }))
      },
      (t, k) => toasts.push([t, k]),
    )
    q.enqueue([file('a.epub', 15 * MB), file('b.epub', 10 * MB), file('notes.pdf'), file('c.epub', 1)])
    await q.idle()
    expect(sent).toEqual([['a.epub'], ['b.epub', 'c.epub']])
    expect(toasts).toEqual([['3 added · 1 skipped (not EPUB)', 'ok']])
    const s = q.get()
    expect(s.busy).toBe(false)
    expect(s.jobs.map((j) => j.state)).toEqual(['done', 'done', 'done'])
    expect(s.results.map((r) => r.filename)).toEqual(['a.epub', 'b.epub', 'c.epub'])
    expect(s.runs).toBe(1)
  })

  it('a drop with no EPUBs uploads nothing and says how many it skipped', async () => {
    const toasts: string[] = []
    let posts = 0
    const q = createUploadQueue(
      async () => {
        posts++
        return []
      },
      (t) => toasts.push(t),
    )
    q.enqueue([file('a.pdf'), file('b.mobi')])
    await q.idle()
    expect(posts).toBe(0)
    expect(toasts).toEqual(['none of those 2 files is an .epub — nothing uploaded'])
    expect(q.get().busy).toBe(false)
  })

  it('a second drop during an upload is queued behind it, not lost', async () => {
    const { posts, upload } = gatedUpload()
    const toasts: string[] = []
    const q = createUploadQueue(upload, (t) => toasts.push(t))
    q.enqueue([file('a.epub')])
    await tick()
    expect(q.get().busy).toBe(true)
    q.enqueue([file('b.epub'), file('x.txt')])
    expect(toasts).toEqual(['queued 1 EPUB — they upload after the current run'])
    expect(q.get().jobs.map((j) => [j.name, j.state])).toEqual([
      ['a.epub', 'uploading'],
      ['b.epub', 'queued'],
    ])
    posts[0].release()
    await tick()
    await tick()
    expect(posts.map((p) => p.names)).toEqual([['a.epub'], ['b.epub']])
    expect(q.get().jobs[1].state).toBe('uploading')
    posts[1].release()
    await q.idle()
    expect(toasts.slice(1)).toEqual(['1 added', '1 added · 1 skipped (not EPUB)'])
    expect(q.get().results.map((r) => r.filename)).toEqual(['a.epub', 'b.epub'])
    expect(q.get().runs).toBe(2)
    expect(q.get().busy).toBe(false)
  })

  it('a failed POST marks every file in its batch failed', async () => {
    const toasts: [string, string | undefined][] = []
    const q = createUploadQueue(
      async () => {
        throw new Error('413 too large')
      },
      (t, k) => toasts.push([t, k]),
    )
    q.enqueue([file('a.epub'), file('b.epub')])
    await q.idle()
    expect(q.get().results.map((r) => r.status)).toEqual(['error', 'error'])
    expect(toasts[0][1]).toBe('warn')
  })
})
