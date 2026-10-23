import { KEYS } from './shortcuts'
import PaletteShell from './PaletteShell'
import { Icon, IconButton, Kbd, SectionLabel } from './lapis'

/** The full shortcut cheatsheet (opened with `?`). The home hint bar shows the
 * common few; this is the complete list, including the editor-only marks. */
export default function ShortcutHelp({ onClose }: { onClose: () => void }) {
  const groups: [string, [string, string][]][] = [
    [
      'Navigate',
      [
        ['⌘O', 'Open a doc or place'],
        ['⌘K', 'Actions (this doc’s first)'],
        ['⌘⇧F', 'Search inside docs (⌘F, ⌘P too)'],
        ['⌘/', 'Ask the library'],
        [KEYS.home, 'Home'],
        [KEYS.tree, 'Show or hide the sidebar'],
        ['⌘,', 'Settings'],
        ['⌘[ / ⌘]', 'Back / forward'],
        ['⌘R', 'Reload'],
      ],
    ],
    [
      'Create & act',
      [
        [KEYS.newdoc, 'New doc'],
        [KEYS.capture, 'Quick capture to Inbox'],
        ['⌥⌘G', 'Capture from anywhere (Mac app)'],
        ['⌘⇧R', 'Review queue'],
        ['⌘G', 'Gardeners'],
      ],
    ],
    [
      'In a doc',
      [
        ['⌘B / ⌘I / ⌘E', 'Bold / italic / code'],
        ['⌘S', 'Confirms it autosaved'],
        ['⌘↵', 'Save now · post a comment'],
        ['Esc', 'Close a panel'],
        ['?', 'This cheatsheet'],
      ],
    ],
  ]
  return (
    <PaletteShell onClose={onClose} className="palette-help" label="Keyboard shortcuts">
      <div className="lp-help">
        <header className="lp-help-head">
          <Icon name="keyboard" size={18} />
          <h2>Keyboard shortcuts</h2>
          <IconButton icon="close" label="Close" size="sm" onClick={onClose} />
        </header>
        <div className="lp-help-groups">
          {groups.map(([title, rows]) => (
            <section className="lp-help-group" key={title} aria-label={title}>
              <SectionLabel>{title}</SectionLabel>
              {rows.map(([keys, label]) => (
                <div className="lp-help-row" key={keys}>
                  <span className="lp-help-label">{label}</span>
                  <span className="lp-help-keys">
                    {keys.split(' ').map((k, i) => (k === '/' ? <span key={i} aria-hidden="true">/</span> : <Kbd key={i}>{k}</Kbd>))}
                  </span>
                </div>
              ))}
            </section>
          ))}
        </div>
      </div>
    </PaletteShell>
  )
}
