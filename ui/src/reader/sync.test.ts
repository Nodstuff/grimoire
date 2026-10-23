import { describe, expect, it, vi } from 'vitest'
import { CSRF_HEADER } from '../auth'
import {
  deviceId,
  fileUrl,
  getPosition,
  isNewer,
  loadPlace,
  type Position,
  PositionWriter,
  putPosition,
  savePlace,
  startTarget,
  syncToast,
} from './sync'

const kindle: Position = { progress: '/body/DocFragment[23]/body/p[1]/text()[1].0', percentage: 0.57, device: 'KindlePaperWhite6', device_id: 'kpw6', timestamp: 200 }
const mine = 'web-1'
const ours: Position = { ...kindle, device: 'Taisce web', device_id: mine, timestamp: 300 }

function memStore() {
  const m = new Map<string, string>()
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m }
}

describe('where the reader opens', () => {
  it("another device's newer place wins, by its XPointer", () => {
    expect(startTarget(kindle, { cfi: 'epubcfi(/6/4)', fraction: 0.1, timestamp: 100 }, mine)).toEqual({
      kind: 'xpointer',
      xpointer: kindle.progress,
      fraction: 0.57,
      from: kindle,
    })
    expect(startTarget(kindle, null, mine).kind).toBe('xpointer')
  })

  it('our own last place stays ours: the finer CFI', () => {
    expect(startTarget(ours, { cfi: 'epubcfi(/6/4!/4/2)', fraction: 0.3, timestamp: 300 }, mine)).toEqual({
      kind: 'cfi',
      cfi: 'epubcfi(/6/4!/4/2)',
      fraction: 0.3,
    })
    // an older device row than what we have seen here: still ours
    expect(startTarget(kindle, { cfi: 'c', fraction: 0.6, timestamp: 250 }, mine).kind).toBe('cfi')
  })

  it('our own row with nothing local (another browser of ours): the XPointer; nothing at all: the start', () => {
    expect(startTarget({ ...ours, device_id: mine }, null, mine).kind).toBe('xpointer')
    expect(startTarget(null, null, mine)).toEqual({ kind: 'start' })
  })
})

describe('the sync toast', () => {
  it("names the device and its percentage, only for another device's place", () => {
    expect(syncToast(kindle, mine)).toBe('Synced from KindlePaperWhite6 · 57%')
    expect(syncToast(ours, mine)).toBeNull()
    expect(syncToast({ ...kindle, device: '', percentage: 1.2 }, mine)).toBe('Synced from another device · 100%')
  })

  it('a return to the tab jumps only for a newer row from elsewhere', () => {
    expect(isNewer(kindle, 100, mine)).toBe(true)
    expect(isNewer(kindle, 200, mine)).toBe(false)
    expect(isNewer(ours, 0, mine)).toBe(false)
    expect(isNewer(null, 0, mine)).toBe(false)
  })
})

describe('this browser', () => {
  it('keeps one device id', () => {
    const s = memStore()
    const a = deviceId(s)
    expect(deviceId(s)).toBe(a)
    expect(a.length).toBeGreaterThan(8)
  })

  it('survives storage that throws: a per-tab id', () => {
    const bad = {
      getItem: () => {
        throw new Error('blocked')
      },
      setItem: () => {
        throw new Error('blocked')
      },
    }
    expect(deviceId(bad)).toBe(deviceId(bad))
  })

  it('remembers a place per book', () => {
    const s = memStore() as unknown as Storage
    expect(loadPlace('book', 'b1', s)).toBeNull()
    savePlace('book', 'b1', { cfi: 'epubcfi(/6/2)', fraction: 0.2, timestamp: 5 }, s)
    expect(loadPlace('book', 'b1', s)).toEqual({ cfi: 'epubcfi(/6/2)', fraction: 0.2, timestamp: 5 })
    expect(loadPlace('doc', 'b1', s)).toBeNull()
  })
})

describe('the routes', () => {
  it('a book streams through the daemon; a doc renders', () => {
    expect(fileUrl('book', 'x')).toBe('/api/books/x/file')
    expect(fileUrl('doc', 'y')).toBe('/api/docs/y/epub')
  })

  it('a 404 means no sync here (LOCAL mode)', async () => {
    const f = vi.fn(async () => new Response('{"error":"not found"}', { status: 404 })) as unknown as typeof fetch
    expect(await getPosition('doc', 'd', f)).toEqual({ ok: false, unavailable: true })
  })

  it('a PUT carries the CSRF header; a 409 hands back the newer row', async () => {
    const calls: RequestInit[] = []
    const f = vi.fn(async (_u: string, init: RequestInit) => {
      calls.push(init)
      return new Response(JSON.stringify({ error: 'newer', position: kindle }), { status: 409 })
    }) as unknown as typeof fetch
    const r = await putPosition('book', 'b', { progress: 'x', percentage: 0.1, device_id: mine, base: 0 }, {}, f)
    expect(r).toEqual({ kind: 'newer', position: kindle })
    expect(new Headers(calls[0].headers).get(CSRF_HEADER)).toBe('1')
    expect(calls[0].method).toBe('PUT')
  })
})

describe('the writer', () => {
  function rig(answer: (b: { progress: string; base: number }) => Position | 'newer' | 'fail' = (b) => ({ ...ours, progress: b.progress, timestamp: 400 })) {
    let t = 0
    const timers: { fn: () => void; at: number }[] = []
    const puts: { progress: string; base: number; keepalive: boolean }[] = []
    const newer: Position[] = []
    const w = new PositionWriter(mine, 200, {
      put: async (b, keepalive) => {
        puts.push({ progress: b.progress, base: b.base, keepalive })
        const a = answer(b)
        if (a === 'newer') return { kind: 'newer', position: { ...kindle, timestamp: 500 } }
        if (a === 'fail') return { kind: 'failed' }
        return { kind: 'written', position: a }
      },
      onNewer: (p) => newer.push(p),
      now: () => t,
      setTimer: (fn, ms) => {
        const e = { fn, at: t + ms }
        timers.push(e)
        return e
      },
      clearTimer: (e) => {
        const i = timers.indexOf(e as (typeof timers)[number])
        if (i >= 0) timers.splice(i, 1)
      },
    })
    const tick = async (ms: number) => {
      t += ms
      for (const e of timers.filter((e) => e.at <= t)) {
        timers.splice(timers.indexOf(e), 1)
        e.fn()
      }
      await new Promise((r) => setTimeout(r, 0))
    }
    return { w, puts, newer, tick, timers }
  }

  it('writes at most every 10 s while reading, the latest place, based on what it saw', async () => {
    const { w, puts, tick } = rig()
    w.moved({ progress: 'p1', percentage: 0.1 })
    await tick(0)
    expect(puts).toEqual([{ progress: 'p1', base: 200, keepalive: false }])
    w.moved({ progress: 'p2', percentage: 0.2 })
    w.moved({ progress: 'p3', percentage: 0.3 })
    await tick(5_000)
    expect(puts.length).toBe(1)
    await tick(5_000)
    // the stamp of its own write is the next base
    expect(puts[1]).toEqual({ progress: 'p3', base: 400, keepalive: false })
  })

  it('flushes at once on hide, with keepalive, and not again for the same place', async () => {
    const { w, puts } = rig()
    w.moved({ progress: 'p1', percentage: 0.1 })
    await w.flush(true)
    expect(puts).toEqual([{ progress: 'p1', base: 200, keepalive: true }])
    await w.flush(true)
    w.moved({ progress: 'p1', percentage: 0.1 })
    await w.flush(true)
    expect(puts.length).toBe(1)
  })

  it('a refused write hands over the newer place and does not retry it', async () => {
    const { w, puts, newer, tick } = rig(() => 'newer')
    w.moved({ progress: 'p1', percentage: 0.1 })
    await tick(0)
    expect(newer.map((p) => p.device)).toEqual(['KindlePaperWhite6'])
    expect(w.pending).toBe(false)
    // the next turn is based on the newer stamp
    w.moved({ progress: 'p2', percentage: 0.2 })
    await w.flush(false)
    expect(puts[1].base).toBe(500)
  })

  it('a failed write stays pending for the next try', async () => {
    let fail = true
    const { w, puts } = rig((b) => (fail ? 'fail' : { ...ours, progress: b.progress, timestamp: 401 }))
    w.moved({ progress: 'p1', percentage: 0.1 })
    await w.flush(false)
    expect(w.pending).toBe(true)
    fail = false
    await w.flush(true)
    expect(puts.map((p) => p.progress)).toEqual(['p1', 'p1'])
    expect(w.pending).toBe(false)
  })

  it('stop() ends writing', async () => {
    const { w, puts, tick } = rig()
    w.stop()
    w.moved({ progress: 'p1', percentage: 0.1 })
    await tick(20_000)
    expect(puts).toEqual([])
  })
})
