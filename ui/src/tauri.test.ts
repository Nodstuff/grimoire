import { afterEach, describe, expect, it } from 'vitest'
import { CAPTURE_EVENT, backupFileName, inTauri, onShellEvent, saveDialog } from './tauri'

type Bridge = {
  invoke: (cmd: string, args?: unknown) => Promise<unknown>
  transformCallback?: (cb: (payload: unknown) => void, once?: boolean) => number
}
const g = globalThis as { __TAURI_INTERNALS__?: Bridge }

describe('tauri bridge', () => {
  afterEach(() => {
    delete g.__TAURI_INTERNALS__
  })

  it('is not in Tauri without the injected bridge, and the save sheet degrades to null', async () => {
    expect(inTauri()).toBe(false)
    expect(await saveDialog({ defaultPath: 'x.db' })).toBeNull()
  })

  it('invokes the dialog plugin and unwraps either reply shape', async () => {
    const calls: [string, unknown][] = []
    let reply: unknown = '/Volumes/Stick/grimoire-2026-09-07.db'
    g.__TAURI_INTERNALS__ = {
      invoke: async (cmd, args) => {
        calls.push([cmd, args])
        return reply
      },
    }
    expect(inTauri()).toBe(true)
    const opts = { title: 'Back up', defaultPath: 'grimoire-2026-09-07.db', filters: [{ name: 'SQLite', extensions: ['db'] }] }
    expect(await saveDialog(opts)).toBe('/Volumes/Stick/grimoire-2026-09-07.db')
    expect(calls).toEqual([['plugin:dialog|save', { options: opts }]])
    reply = { path: '/tmp/a.db' }
    expect(await saveDialog(opts)).toBe('/tmp/a.db')
    // cancel
    reply = null
    expect(await saveDialog(opts)).toBeNull()
  })

  it('shell events: listen through the bridge, deliver the payload, unlisten by id', async () => {
    // no bridge → inert
    expect(typeof onShellEvent(CAPTURE_EVENT, () => {})).toBe('function')
    const calls: [string, unknown][] = []
    const callbacks = new Map<number, (p: unknown) => void>()
    g.__TAURI_INTERNALS__ = {
      invoke: async (cmd, args) => {
        calls.push([cmd, args])
        return cmd === 'plugin:event|listen' ? 42 : null
      },
      transformCallback: (cb) => {
        callbacks.set(7, cb)
        return 7
      },
    }
    const got: unknown[] = []
    const off = onShellEvent(CAPTURE_EVENT, (p) => got.push(p))
    await Promise.resolve()
    expect(calls[0]).toEqual(['plugin:event|listen', { event: 'grimoire:capture', target: { kind: 'Any' }, handler: 7 }])
    callbacks.get(7)!({ event: CAPTURE_EVENT, id: 42, payload: { from: 'hotkey' } })
    expect(got).toEqual([{ from: 'hotkey' }])
    off()
    await new Promise((r) => setTimeout(r, 0))
    expect(calls[1]).toEqual(['plugin:event|unlisten', { event: 'grimoire:capture', eventId: 42 }])
  })

  it('proposes a dated .db name, zero-padded', () => {
    expect(backupFileName(new Date(2026, 8, 7))).toBe('grimoire-2026-09-07.db')
    expect(backupFileName(new Date(2026, 11, 25))).toBe('grimoire-2026-12-25.db')
  })
})
