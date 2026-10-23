// An e-reader (canvas Books / Settings device rows). Kept beside the screens
// that use it rather than in the shared icon set.
export function DeviceIcon({ color = 'var(--ink2)' }: { color?: string }) {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" style={{ color, flex: 'none' }} aria-hidden focusable="false">
      <rect x="6" y="3" width="12" height="18" rx="2" />
      <path d="M9 7h6M9 10h6M9 13h4" />
    </svg>
  )
}
