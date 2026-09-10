import { describe, expect, it } from 'vitest'
import { fmtElapsed, fmtTokens, parseProgress, parseSummary, runElapsed, runTone, verdictChips } from './runs'

describe('runTone', () => {
  it('maps every status the daemon writes', () => {
    expect(runTone('ok')).toBe('ok')
    expect(runTone('failed')).toBe('failed')
    expect(runTone('budget-killed')).toBe('warn')
    expect(runTone('orphaned')).toBe('warn')
    expect(runTone('running')).toBe('running')
    expect(runTone('weird')).toBe('other')
  })
})

describe('parseProgress', () => {
  it('reads the live line and nothing else', () => {
    expect(parseProgress('working… 41s · 12 tool calls · last: Read /Users/x/y.md')).toEqual({
      seconds: 41,
      toolCalls: 12,
      last: 'Read /Users/x/y.md',
    })
    expect(parseProgress('working… 3s · 1 tool call')).toEqual({ seconds: 3, toolCalls: 1, last: '' })
    expect(parseProgress('docs considered: 3; verdicts green 0, yellow 1, red 0')).toBeNull()
    expect(parseProgress(null)).toBeNull()
  })
})

describe('parseSummary', () => {
  it('tagging: docs + non-zero verdict chips, log lines behind', () => {
    const s = parseSummary('docs considered: 10; verdicts green 0, yellow 9, red 0\nRoadmap: yellow — tags a, b\nNotes: yellow — tags c')
    expect(s.chips).toEqual([{ text: '10 docs' }, { text: '9 yellow', tone: 'yellow' }])
    expect(s.headline).toBeNull()
    expect(s.lines).toEqual(['Roadmap: yellow — tags a, b', 'Notes: yellow — tags c'])
  })
  it('auditor and filer shapes', () => {
    expect(parseSummary('docs audited: 5; parked fixes: 0, verified fixes: 6\nx').chips).toEqual([
      { text: '5 audited' },
      { text: '6 verified', tone: 'yellow' },
    ])
    expect(parseSummary('notes considered: 3; filed 2, renamed 1, tagged 3').chips).toEqual([
      { text: '3 notes' },
      { text: '2 filed', tone: 'yellow' },
      { text: '1 renamed' },
      { text: '3 tagged' },
    ])
  })
  it('nothing to do carries its reason as the headline', () => {
    const s = parseSummary('nothing to do: the Inbox is empty')
    expect(s.chips).toEqual([{ text: 'nothing to do' }])
    expect(s.headline).toBe('the Inbox is empty')
  })
  it('anything else is a headline (a failure message)', () => {
    const s = parseSummary('claude exited 1: budget exceeded\nstderr line')
    expect(s.chips).toEqual([])
    expect(s.headline).toBe('claude exited 1: budget exceeded')
    expect(s.lines).toEqual(['stderr line'])
    expect(parseSummary(null)).toEqual({ chips: [], headline: null, lines: [] })
  })
})

describe('verdictChips / elapsed / tokens', () => {
  it('pulls non-zero verdict counts from free text', () => {
    expect(verdictChips('verdicts: 3 yellow, 0 red, 2 green')).toEqual([
      { text: '3 yellow', tone: 'yellow' },
      { text: '2 green', tone: 'green' },
    ])
    expect(verdictChips('nothing')).toEqual([])
  })
  it('formats elapsed and tokens compactly', () => {
    expect(fmtElapsed(41)).toBe('41s')
    expect(fmtElapsed(185)).toBe('3m 05s')
    expect(fmtElapsed(4380)).toBe('1h 13m')
    expect(fmtTokens(950)).toBe('950')
    expect(fmtTokens(12_340)).toBe('12.3k')
    expect(fmtTokens(2_000)).toBe('2k')
    expect(fmtTokens(250_000)).toBe('250k')
    expect(fmtTokens(null)).toBe('')
  })
  it('elapsed prefers the progress line, else the clock', () => {
    const run = { id: 'r', gardener: 'g', gardener_name: 'n', started_at: '2026-09-10T10:00:00Z', status: 'running', summary: 'working… 41s · 2 tool calls', tokens_used: null, tool_calls: null }
    expect(runElapsed(run, Date.parse('2026-09-10T10:05:00Z'))).toBe(41)
    expect(runElapsed({ ...run, summary: null }, Date.parse('2026-09-10T10:05:00Z'))).toBe(300)
  })
})
