// Small shared pieces for showing a gardener run (Home's timeline and the
// Gardeners page): the status pill (green dot / red / amber / spinner), the
// headline chips parsed from the summary, and the live progress line.

import { fmtElapsed, parseProgress, parseSummary, runElapsed, runTone, verdictChips, type RunChip } from './runs'
import type { GardenerRun } from './types'

export function StatusPill({ run, now }: { run: GardenerRun; now?: number }) {
  const tone = runTone(run.status)
  const label =
    tone === 'running' ? `running ${fmtElapsed(runElapsed(run, now))}` : run.status === 'ok' ? 'ok' : run.status.replace('-', ' ')
  return (
    <span className={`run-pill ${tone}`} title={run.status}>
      <span className="run-dot" aria-hidden />
      {label}
    </span>
  )
}

export function Chips({ chips }: { chips: RunChip[] }) {
  if (chips.length === 0) return null
  return (
    <span className="run-chips">
      {chips.map((c, i) => (
        <span key={`${c.text}-${i}`} className={`run-chip ${c.tone ?? ''}`}>
          {c.text}
        </span>
      ))}
    </span>
  )
}

/** Chips for a finished run: the known shapes, else any verdict counts. */
export function runChips(run: GardenerRun): RunChip[] {
  if (run.status === 'running') return []
  const s = parseSummary(run.summary)
  if (s.chips.length) return s.chips
  return verdictChips(run.summary)
}

/** `working… 41s · 12 tool calls · last: Read …` as its own pulsing line. */
export function ProgressLine({ run }: { run: GardenerRun }) {
  const p = parseProgress(run.summary)
  if (!p) return <div className="run-progress">working…</div>
  return (
    <div className="run-progress">
      working… {fmtElapsed(p.seconds)} · {p.toolCalls} tool call{p.toolCalls === 1 ? '' : 's'}
      {p.last && <span className="run-progress-last"> · last: {p.last}</span>}
    </div>
  )
}

/** The failure line of a failed / killed run (the summary's first line). */
export function failureLine(run: GardenerRun): string | null {
  const tone = runTone(run.status)
  if (tone !== 'failed' && tone !== 'warn') return null
  return parseSummary(run.summary).headline ?? run.status
}
