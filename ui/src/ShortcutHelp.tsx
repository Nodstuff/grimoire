import PaletteShell from './PaletteShell'

/** The full shortcut cheatsheet (opened with `?`). The home hint bar shows the
 * common few; this is the complete list, including the editor-only marks. */
export default function ShortcutHelp({ onClose }: { onClose: () => void }) {
  const groups: [string, [string, string][]][] = [
    [
      'Navigate',
      [
        ['⌘O', 'open a doc or place — fuzzy over titles and paths (recent when empty)'],
        ['⌘K', 'actions — new doc, capture, export, backup, sign out… (this doc’s first)'],
        ['⌘⇧F', 'search inside docs (⌘F and ⌘P too; ⌘S in a doc just confirms autosave)'],
        ['⌘/', 'ask the library — an answer doc with block citations (? in search; ⌘↵ asks)'],
        ['⌘,', 'settings — profile, e-readers'],
        ['⌘T', 'show or hide the sidebar'],
        ['⌘W', 'home'],
        ['⌘[ / ⌘]', 'history back / forward'],
        ['⌘R', 'reload'],
      ],
    ],
    [
      'Create & act',
      [
        ['⌘N', 'new doc'],
        ['⌘⇧I', 'quick capture → Inbox (⌥⌘G from anywhere on the Mac, in the app)'],
        ['⌘⇧R', 'review queue'],
        ['⌘G', 'gardeners'],
      ],
    ],
    [
      'In a doc',
      [
        ['⌘B / ⌘I / ⌘E', 'bold / italic / code'],
        ['⌘↵', 'save now / post comment'],
        ['Esc', 'close panel'],
        ['?', 'this cheatsheet'],
      ],
    ],
  ]
  return (
    <PaletteShell onClose={onClose}>
      <div className="shortcut-help">
        <div className="shortcut-help-title">Keyboard shortcuts</div>
        {groups.map(([title, rows]) => (
          <div className="shortcut-group" key={title}>
            <div className="shortcut-group-title">{title}</div>
            {rows.map(([keys, label]) => (
              <div className="shortcut-row" key={keys}>
                <span className="shortcut-keys">
                  {keys.split(' ').map((k, i) =>
                    k === '/' ? (
                      <span key={i}> / </span>
                    ) : (
                      <kbd key={i}>{k}</kbd>
                    ),
                  )}
                </span>
                <span className="shortcut-label">{label}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </PaletteShell>
  )
}
