// The typographic cover a book without its own cover image wears (canvas
// Books): a cloth colour, an ink colour and one quiet motif, picked from
// the book's id so the same book always looks the same. The palettes are
// artwork lifted from the artboard, not interface colours, so they are not
// tokens and do not change with the theme (a book's cover does not either).

import type { CSSProperties } from 'react'

export interface CoverArt {
  bg: string
  fg: string
  motif: CSSProperties
}

const ART: CoverArt[] = [
  { bg: '#1F3A4D', fg: '#F1E6CF', motif: { right: -34, bottom: -34, width: '85%', aspectRatio: '1', borderRadius: '50%', border: '10px solid rgba(241,230,207,.22)' } },
  { bg: '#E6E0D2', fg: '#1B2430', motif: { left: 16, right: 16, bottom: '22%', height: '18%', background: 'repeating-linear-gradient(90deg,#1B2430 0 3px,transparent 3px 11px)', opacity: 0.75 } },
  { bg: '#2F5D46', fg: '#EAF2DC', motif: { right: -20, top: '46%', width: '72%', aspectRatio: '1', borderRadius: '50% 50% 0 50%', background: 'rgba(234,242,220,.16)' } },
  { bg: '#F2C14E', fg: '#2A1F05', motif: { right: -40, top: '30%', width: '92%', aspectRatio: '1', borderRadius: '50%', background: 'rgba(255,255,255,.35)' } },
  { bg: '#DCE3EA', fg: '#1B2430', motif: { left: 0, right: 0, bottom: '15%', height: '26%', background: 'repeating-linear-gradient(0deg,rgba(27,36,48,.12) 0 2px,transparent 2px 9px)' } },
  { bg: '#2B2440', fg: '#F4D9C6', motif: { left: '20%', top: '52%', width: 6, height: 6, borderRadius: '50%', background: '#F4D9C6', boxShadow: '30px -20px 0 -1px #F4D9C6,58px 10px 0 -2px #F4D9C6,14px 30px 0 -2px #F4D9C6' } },
  { bg: '#7A2E2A', fg: '#F6E7D7', motif: { left: 16, right: 16, top: '48%', height: 1, background: 'rgba(246,231,215,.5)' } },
  { bg: '#23395B', fg: '#E9EEF6', motif: { left: -20, right: -20, bottom: '-30%', height: '60%', borderRadius: '50%', border: '2px solid rgba(233,238,246,.3)' } },
  { bg: '#C9622F', fg: '#FFF1E3', motif: { left: '50%', top: '40%', width: '52%', aspectRatio: '1', transform: 'translateX(-50%)', border: '2px solid rgba(255,241,227,.45)', borderRadius: '50%', boxShadow: '24px 0 0 -2px rgba(255,241,227,.2)' } },
  { bg: '#A9C3B5', fg: '#16302A', motif: { right: 18, top: '44%', width: '28%', height: '30%', borderRadius: '22px 22px 4px 4px', background: 'rgba(22,48,42,.14)' } },
  { bg: '#3B4A2A', fg: '#E9DFB8', motif: { left: 10, right: 10, bottom: '18%', height: '32%', background: 'linear-gradient(135deg,transparent 49%,rgba(233,223,184,.3) 50%) left/50% 100% no-repeat,linear-gradient(225deg,transparent 49%,rgba(233,223,184,.3) 50%) right/50% 100% no-repeat' } },
  { bg: '#F4F1EA', fg: '#C2412D', motif: { left: 16, right: 16, bottom: '20%', height: '30%', background: 'linear-gradient(#C2412D 0 0) 0 0/100% 1px,linear-gradient(#C2412D 0 0) 0 50%/100% 1px,linear-gradient(#C2412D 0 0) 0 100%/100% 1px', backgroundRepeat: 'no-repeat', opacity: 0.5 } },
]

/** FNV-1a over the id: stable across reloads and machines. */
function hash(s: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

/** The cover art for a book id (the same id, the same cover). */
export function coverArt(id: string): CoverArt {
  return ART[hash(id) % ART.length]
}

export const COVER_ART_COUNT = ART.length
