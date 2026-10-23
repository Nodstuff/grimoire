// Server-render smoke for the navigation surfaces: each palette mounts, shows
// only its own kind of row, and carries its key hints.
import { describe, expect, it } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import OpenPalette from './OpenPalette'
import ActionsPalette from './ActionsPalette'
import SearchPalette from './SearchPalette'
import Settings from './Settings'
import { buildPlaces } from './omni'
import { buildCommands } from './commands'
import type { Doc } from './types'

const doc = (id: string, title: string): Doc => ({
  id,
  title,
  parent_id: null,
  review_policy: null,
  current_epoch: 1,
  created_by: 'p',
  status: null,
  sort_key: null,
})
const noop = () => {}

describe('palettes render', () => {
  it('Open: places listed, footer hints, no actions', () => {
    const html = renderToString(
      createElement(OpenPalette, {
        docs: [doc('a', 'Roadmap')],
        places: buildPlaces({ library: true }),
        onOpenDoc: noop,
        onPlace: noop,
        onSearchText: noop,
        onClose: noop,
      }),
    )
    expect(html).toContain('Places')
    expect(html).toContain('Books')
    expect(html).toContain('Settings')
    expect(html).toContain('⌘↵')
    expect(html).not.toContain('New doc')
  })
  it('Actions: this doc first, keys on rows, no places', () => {
    const html = renderToString(
      createElement(ActionsPalette, {
        commands: buildCommands({ signedIn: true, library: true, onSignOut: noop, docId: 'a', onAction: noop }),
        onClose: noop,
      }),
    )
    expect(html.indexOf('This doc')).toBeLessThan(html.indexOf('New doc'))
    expect(html).toContain('<kbd>⌘N</kbd>')
    expect(html).not.toContain('Trash')
  })
  it('Search: ? opens it asking', () => {
    const html = renderToString(createElement(SearchPalette, { initial: '? ', onOpenDoc: noop, onClose: noop }))
    expect(html).toContain('Ask your notes')
  })
  it('Settings: E-readers and Upload tokens tabs in SERVER mode only', () => {
    const props = { section: 'profile' as const, onSection: noop, dataVersion: 0, version: null, onProfileChanged: noop }
    const server = renderToString(createElement(Settings, { ...props, server: true }))
    expect(server).toContain('E-readers')
    expect(server).toContain('Upload tokens')
    const local = renderToString(createElement(Settings, { ...props, server: false }))
    expect(local).not.toContain('E-readers')
    expect(local).not.toContain('Upload tokens')
  })
  it('Settings › Upload tokens renders its add card', () => {
    const props = { onSection: noop, dataVersion: 0, version: null, onProfileChanged: noop }
    const html = renderToString(createElement(Settings, { ...props, section: 'uploads' as const, server: true }))
    expect(html).toContain('add an upload token')
  })
})
