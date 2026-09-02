// The centred modal chrome shared by every palette (⌘K, ⌘O, ⌘P, help) and
// the first-run name prompt. `locked` disables the click-outside dismissal.

export default function PaletteShell({
  children,
  onClose,
  locked = false,
}: {
  children: React.ReactNode
  onClose: () => void
  /** true → clicking the backdrop does nothing (caller also owns Esc) */
  locked?: boolean
}) {
  return (
    <div className="palette-backdrop" onMouseDown={locked ? undefined : onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>
  )
}
