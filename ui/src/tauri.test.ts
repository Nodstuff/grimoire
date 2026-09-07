import { afterEach, describe, expect, it } from 'vitest'
import { backupFileName, inTauri, saveDialog } from './tauri'

type Bridge = { invoke: (cmd: string, args?: unknown) => Promise<unknown> }
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

  it('proposes a dated .db name, zero-padded', () => {
    expect(backupFileName(new Date(2026, 8, 7))).toBe('grimoire-2026-09-07.db')
    expect(backupFileName(new Date(2026, 11, 25))).toBe('grimoire-2026-12-25.db')
  })
})
