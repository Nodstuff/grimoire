// Inline stroke icons (24×24 grid, 1.75 stroke, round caps), paths lifted
// from the Lapis canvas. No emoji anywhere in the interface: use these.
// <Icon name="home" /> is decorative by default (aria-hidden); pass `label`
// when the icon alone carries meaning.

import type { CSSProperties } from 'react'

const PATHS = {
  home: <path d="M4 10.5 12 4l8 6.5V20a1 1 0 0 1-1 1h-5v-6h-4v6H5a1 1 0 0 1-1-1z" />,
  inbox: (
    <>
      <path d="M4 13h4l2 3h4l2-3h4" />
      <path d="M5.5 5h13L20 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-6z" />
    </>
  ),
  todo: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="4" />
      <path d="m8.5 12 2.5 2.5 4.5-5" />
    </>
  ),
  review: (
    <>
      <path d="M12 3 4.5 6v5.5c0 4.4 3.1 8.1 7.5 9.5 4.4-1.4 7.5-5.1 7.5-9.5V6z" />
      <path d="m9 12 2 2 4-4" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m20 20-4.2-4.2" />
    </>
  ),
  books: (
    <>
      <path d="M5 4h4v16H5zM10 4h4v16h-4z" />
      <path d="m15.5 5.2 3.4-.9 3.4 14.6-3.4.9z" />
    </>
  ),
  folder: <path d="M3.5 7a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" />,
  doc: (
    <>
      <path d="M7 3.5h7l4.5 4.5V20a.5.5 0 0 1-.5.5H7a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5z" />
      <path d="M14 3.5V8h4.5" />
    </>
  ),
  docPlus: (
    <>
      <path d="M7 3.5h7l4.5 4.5V20a.5.5 0 0 1-.5.5H7a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5z" />
      <path d="M14 3.5V8h4.5M12.5 11v6M9.5 14h6" />
    </>
  ),
  gardeners: (
    <>
      <path d="M12 20v-8" />
      <path d="M12 12c0-4 3-6 7-6 0 4-3 6-7 6zM12 14c0-3-2.5-5-6.5-5 0 3 2.5 5 6.5 5z" />
    </>
  ),
  trash: <path d="M4.5 7h15M10 4h4M6.5 7l1 13h9l1-13" />,
  settings: (
    <>
      <path d="M4 7h10M18 7h2M4 17h4M12 17h8" />
      <circle cx="16" cy="7" r="2" />
      <circle cx="10" cy="17" r="2" />
    </>
  ),
  sidebar: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
      <path d="M9.5 4.5v15" />
    </>
  ),
  menu: <path d="M4 7h16M4 12h16M4 17h16" />,
  back: <path d="m14.5 6-6 6 6 6" />,
  forward: <path d="m9.5 6 6 6-6 6" />,
  chevronRight: <path d="m9.5 6 6 6-6 6" />,
  chevronDown: <path d="m7 10 5 5 5-5" />,
  chevronUp: <path d="m7 14 5-5 5 5" />,
  plus: <path d="M12 5v14M5 12h14" />,
  close: <path d="M6 6l12 12M18 6 6 18" />,
  check: <path d="m5 12.5 4.5 4.5L19 7.5" />,
  more: (
    <>
      <circle cx="5.5" cy="12" r="1" />
      <circle cx="12" cy="12" r="1" />
      <circle cx="18.5" cy="12" r="1" />
    </>
  ),
  capture: (
    <>
      <path d="M4 20l4-1 11-11-3-3L5 16z" />
      <path d="m14 7 3 3" />
    </>
  ),
  actions: <path d="m13 3-8 11h6l-1 7 8-11h-6z" />,
  ask: <path d="M12 3.5 13.8 10.2 20.5 12 13.8 13.8 12 20.5 10.2 13.8 3.5 12 10.2 10.2z" />,
  link: (
    <>
      <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" />
      <path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" />
    </>
  ),
  copy: (
    <>
      <rect x="8" y="8" width="12" height="12" rx="2" />
      <path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3" />
    </>
  ),
  download: (
    <>
      <path d="M12 4v11M8 11l4 4 4-4" />
      <path d="M5 19h14" />
    </>
  ),
  upload: (
    <>
      <path d="M12 15V4M8 8l4-4 4 4" />
      <path d="M5 19h14" />
    </>
  ),
  history: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  keyboard: (
    <>
      <rect x="3" y="6" width="18" height="12" rx="2" />
      <path d="M7 10h1M11 10h2M16 10h1M8 14h8" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M5.6 18.4 7 17M17 7l1.4-1.4" />
    </>
  ),
  moon: <path d="M19.5 14.5A8 8 0 0 1 9.5 4.5a8 8 0 1 0 10 10z" />,
  system: (
    <>
      <rect x="3.5" y="4.5" width="17" height="12" rx="2" />
      <path d="M9 20h6M12 16.5V20" />
    </>
  ),
  workspace: <rect x="4" y="4" width="16" height="16" rx="5" />,
  members: (
    <>
      <circle cx="9" cy="9" r="3.5" />
      <path d="M3 20c.6-3.3 3-5 6-5s5.4 1.7 6 5" />
      <path d="M16 5.5a3.5 3.5 0 0 1 0 7M18 15c1.6.6 2.7 2.2 3 5" />
    </>
  ),
  graph: (
    <>
      <circle cx="6" cy="7" r="2.5" />
      <circle cx="18" cy="6" r="2.5" />
      <circle cx="12" cy="18" r="2.5" />
      <path d="M8.3 8.2 10.8 16M16.3 7.8 13.2 16M8.5 6.8l7-.6" />
    </>
  ),
  /* verdicts: the shape carries the meaning as much as the colour */
  applied: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="m8.5 12 2.5 2.5 4.5-5" />
    </>
  ),
  flagged: <path d="M5 21V4M5 4h11l-2 4 2 4H5" />,
  parked: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="4" />
      <path d="M10 9v6M14 9v6" />
    </>
  ),
  warning: (
    <>
      <path d="M12 4 2.8 19.5h18.4z" />
      <path d="M12 10v4.5M12 17v.5" />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 11v5M12 8v.5" />
    </>
  ),
  error: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M9 9l6 6M15 9l-6 6" />
    </>
  ),
  external: (
    <>
      <path d="M14 5h5v5M19 5l-8 8" />
      <path d="M17 14v4a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1h4" />
    </>
  ),
  signOut: (
    <>
      <path d="M14 4h4a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1h-4" />
      <path d="M10 8l-4 4 4 4M6 12h9" />
    </>
  ),
  comment: <path d="M5 5h14a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-7l-4 3.5V16H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z" />,
  edit: (
    <>
      <path d="M4 20l4-1 11-11-3-3L5 16z" />
    </>
  ),
} as const

export type IconName = keyof typeof PATHS
export const ICON_NAMES = Object.keys(PATHS) as IconName[]

export function Icon({
  name,
  size = 18,
  label,
  className,
  style,
  strokeWidth = 1.75,
}: {
  name: IconName
  /** px; 18 in nav rows, 15 in small buttons and palette rows */
  size?: number
  /** read aloud; omit for decorative icons beside a word */
  label?: string
  className?: string
  style?: CSSProperties
  strokeWidth?: number
}) {
  return (
    <svg
      className={`lp-icon${className ? ` ${className}` : ''}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={style}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  )
}

/** The agent glyph: a filled four-point star (in the violet square). */
export function AgentGlyph({ size = 13 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden focusable="false">
      <path d="M12 2.5 14 10l7.5 2-7.5 2-2 7.5-2-7.5L2.5 12 10 10z" />
    </svg>
  )
}

/** The ◈ mark: lapis diamond, a ground-coloured inset, a lapis core. The
 * inset takes the surface it sits on (`--panel` in the sidebar). */
export function LapisMark({ size = 26, inset = 'var(--panel)' }: { size?: number; inset?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden focusable="false" className="lp-mark">
      <rect x="5" y="5" width="22" height="22" rx="5" transform="rotate(45 16 16)" style={{ fill: 'var(--accent)' }} />
      <rect x="11" y="11" width="10" height="10" rx="2" transform="rotate(45 16 16)" style={{ fill: inset }} />
      <rect x="13.6" y="13.6" width="4.8" height="4.8" rx="1" transform="rotate(45 16 16)" style={{ fill: 'var(--accent)' }} />
    </svg>
  )
}
