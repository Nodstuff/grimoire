// Pure seams for rendering gardener runs (the Gardeners page and the home's
// "since you were here" timeline): a run's status tone, the headline chips
// parsed out of the known summary shapes, the live progress line of a
// running run, and the per-item log behind the disclosure. No DOM, no fetch.

import type { GardenerRun } from './types'

export type RunTone = 'ok' | 'failed' | 'warn' | 'running' | 'other'

/** ok → green, failed → red, budget-killed/orphaned → amber, running → spinner. */
export function runTone(status: string): RunTone {
  switch (status) {
    case 'ok':
      return 'ok'
    case 'failed':
      return 'failed'
    case 'budget-killed':
    case 'orphaned':
      return 'warn'
    case 'running':
      return 'running'
    default:
      return 'other'
  }
}

export interface RunChip {
  text: string
  /** verdict colour, when the chip is a verdict count */
  tone?: 'green' | 'yellow' | 'red'
}

export interface RunProgress {
  seconds: number
  toolCalls: number
  /** "Read …" — the last tool and its first argument, may be empty */
  last: string
}

/** `working… 41s · 12 tool calls · last: Read /x` → parts; null otherwise. */
export function parseProgress(summary: string | null | undefined): RunProgress | null {
  if (!summary) return null
  const m = /^working…\s*(\d+)s\s*·\s*(\d+)\s*tool calls?\s*(?:·\s*last:\s*(.*))?$/m.exec(summary.trim())
  if (!m) return null
  return { seconds: Number(m[1]), toolCalls: Number(m[2]), last: (m[3] ?? '').trim() }
}

export interface RunSummary {
  /** headline chips: `10 docs`, `9 yellow`, `nothing to do`, … */
  chips: RunChip[]
  /** the first line when no shape matched (a failure message, free text) */
  headline: string | null
  /** the per-item log: every line after the headline */
  lines: string[]
}

/** Split a finished run's summary into chips + the detail lines. Known shapes:
 * - `docs considered: 10; verdicts green 0, yellow 9, red 0` (tagging)
 * - `docs audited: 5; parked fixes: 0, verified fixes: 6` (auditor)
 * - `notes considered: 3; filed 2, renamed 1, tagged 3` (filer)
 * - `nothing to do: …`
 * Zero counts are dropped from the chips; the numbers are in the log. */
export function parseSummary(summary: string | null | undefined): RunSummary {
  if (!summary) return { chips: [], headline: null, lines: [] }
  const [first = '', ...rest] = summary.split('\n')
  const lines = rest.filter((l) => l.trim().length > 0)
  const head = first.trim()
  const chips: RunChip[] = []
  let m: RegExpExecArray | null
  if ((m = /^docs considered:\s*(\d+);\s*verdicts\s+green\s+(\d+),\s*yellow\s+(\d+),\s*red\s+(\d+)/i.exec(head))) {
    chips.push({ text: `${m[1]} doc${m[1] === '1' ? '' : 's'}` })
    if (m[2] !== '0') chips.push({ text: `${m[2]} green`, tone: 'green' })
    if (m[3] !== '0') chips.push({ text: `${m[3]} yellow`, tone: 'yellow' })
    if (m[4] !== '0') chips.push({ text: `${m[4]} red`, tone: 'red' })
    return { chips, headline: null, lines }
  }
  if ((m = /^docs audited:\s*(\d+);\s*parked fixes:\s*(\d+),\s*verified fixes:\s*(\d+)/i.exec(head))) {
    chips.push({ text: `${m[1]} audited` })
    if (m[3] !== '0') chips.push({ text: `${m[3]} verified`, tone: 'yellow' })
    if (m[2] !== '0') chips.push({ text: `${m[2]} parked`, tone: 'red' })
    return { chips, headline: null, lines }
  }
  if ((m = /^notes considered:\s*(\d+);\s*filed\s+(\d+),\s*renamed\s+(\d+),\s*tagged\s+(\d+)/i.exec(head))) {
    chips.push({ text: `${m[1]} note${m[1] === '1' ? '' : 's'}` })
    if (m[2] !== '0') chips.push({ text: `${m[2]} filed`, tone: 'yellow' })
    if (m[3] !== '0') chips.push({ text: `${m[3]} renamed` })
    if (m[4] !== '0') chips.push({ text: `${m[4]} tagged` })
    return { chips, headline: null, lines }
  }
  if (/^nothing to do\b/i.test(head)) {
    const why = head.replace(/^nothing to do:?\s*/i, '')
    return { chips: [{ text: 'nothing to do' }], headline: why || null, lines }
  }
  return { chips: [], headline: head || null, lines }
}

/** Verdict chips out of any text naming counts (`3 yellow, 1 red`) — the
 * fallback for summaries the shapes above do not cover. */
export function verdictChips(text: string | null | undefined): RunChip[] {
  if (!text) return []
  const out: RunChip[] = []
  for (const m of text.matchAll(/(\d+)\s+(green|yellow|red)\b/gi)) {
    if (m[1] !== '0') out.push({ text: `${m[1]} ${m[2].toLowerCase()}`, tone: m[2].toLowerCase() as RunChip['tone'] })
  }
  return out
}

/** `41s`, `3m 05s`, `1h 12m` for a run's wall time. */
export function fmtElapsed(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  const m = Math.floor(seconds / 60)
  const s = seconds % 60
  if (m < 60) return `${m}m ${String(s).padStart(2, '0')}s`
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`
}

/** Seconds since the run started (the progress line's own count when present). */
export function runElapsed(run: GardenerRun, now: number = Date.now()): number {
  const p = parseProgress(run.summary)
  if (p) return p.seconds
  return Math.max(0, Math.floor((now - new Date(run.started_at).getTime()) / 1000))
}

/** `12.3k` for token counts. */
export function fmtTokens(n: number | null | undefined): string {
  if (n == null) return ''
  if (n < 1000) return `${n}`
  if (n < 100_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`
  return `${Math.round(n / 1000)}k`
}
