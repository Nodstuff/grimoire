import { describe, expect, it } from 'vitest'
import { addDays, deadlineTone, fmtDay, fmtDeadline, hasDuePhrase, isoDate, nextMonday, openItems, previewLabel, summaryLine, type TodoItem } from './todo'

const item = (over: Partial<TodoItem> = {}): TodoItem => ({
  id: '0-abc',
  text: 'x',
  done: false,
  overdue: false,
  due_soon: false,
  ...over,
})

describe('dates', () => {
  it('formats local dates and steps across month and year ends', () => {
    expect(isoDate(new Date(2026, 8, 10))).toBe('2026-09-10')
    expect(addDays('2026-09-30', 1)).toBe('2026-10-01')
    expect(addDays('2026-01-01', -1)).toBe('2025-12-31')
    expect(addDays('2026-09-10', 2)).toBe('2026-09-12')
  })
  it('next Monday is strictly after the date', () => {
    expect(nextMonday('2026-09-10')).toBe('2026-09-14') // Thursday
    expect(nextMonday('2026-09-14')).toBe('2026-09-21') // a Monday → the following one
    expect(nextMonday('2026-09-13')).toBe('2026-09-14') // Sunday
    expect(nextMonday('2026-09-12')).toBe('2026-09-14') // Saturday
  })
  it('names today / tomorrow / yesterday, else a short date', () => {
    expect(fmtDay('2026-09-10', '2026-09-10')).toBe('today')
    expect(fmtDay('2026-09-11', '2026-09-10')).toBe('tomorrow')
    expect(fmtDay('2026-09-09', '2026-09-10')).toBe('yesterday')
    expect(fmtDay('2026-09-20', '2026-09-10')).toMatch(/20/)
  })
})

describe('deadlines', () => {
  it('tone: overdue > soon > later, done never nags', () => {
    expect(deadlineTone(item({ overdue: true, due_soon: false }))).toBe('overdue')
    expect(deadlineTone(item({ due_soon: true }))).toBe('soon')
    expect(deadlineTone(item())).toBe('later')
    expect(deadlineTone(item({ overdue: true, done: true }))).toBe('later')
  })
  it('pill text: `due Fri` this week, `due 12 Sep` later, `2d overdue`', () => {
    expect(fmtDeadline('2026-09-10', '2026-09-10')).toBe('due today')
    expect(fmtDeadline('2026-09-11', '2026-09-10')).toBe('due tomorrow')
    expect(fmtDeadline('2026-09-07', '2026-09-10')).toBe('3d overdue')
    expect(fmtDeadline('2026-09-08', '2026-09-10')).toBe('2d overdue')
    expect(fmtDeadline('2026-09-13', '2026-09-10')).toMatch(/^due \w{3}$/)
    expect(fmtDeadline('2026-09-16', '2026-09-10')).toMatch(/^due \w{3}$/) // 6 days out: still a weekday
    expect(fmtDeadline('2026-09-17', '2026-09-10')).toMatch(/^due .*17/) // a week out: the date
    expect(fmtDeadline('2026-10-13', '2026-09-10')).toMatch(/^due .*13/)
    expect(fmtDeadline('2026-10-13', '2026-09-10')).not.toContain('⏰')
  })
})

describe('due phrase hint', () => {
  it('asks the daemon only for a trailing due/by phrase of up to three words', () => {
    expect(hasDuePhrase('call the bank due fri')).toBe(true)
    expect(hasDuePhrase('call the bank by 12/9')).toBe(true)
    expect(hasDuePhrase('call the bank DUE 12 sep 2027')).toBe(true)
    expect(hasDuePhrase('call the bank due in 3 days')).toBe(true)
    expect(hasDuePhrase('due tomorrow')).toBe(true)
    expect(hasDuePhrase('call the bank due ')).toBe(false) // nothing after it yet
    expect(hasDuePhrase('call the bank')).toBe(false)
    expect(hasDuePhrase('overdue fri')).toBe(false) // not the word
    expect(hasDuePhrase('due fri call the bank now please')).toBe(false) // not trailing
  })
  it('renders `→ <day>` for a deadline, the warning as is, nothing otherwise', () => {
    expect(previewLabel(null, '2026-09-10')).toBe('')
    expect(previewLabel({ text: 'x', deadline: null }, '2026-09-10')).toBe('')
    expect(previewLabel({ text: 'x', deadline: null, warning: "couldn't read that date" }, '2026-09-10')).toBe("couldn't read that date")
    const later = previewLabel({ text: 'x', deadline: '2026-09-18' }, '2026-09-10')
    expect(later).toMatch(/^→ /)
    expect(later).toMatch(/18/)
    expect(later).toMatch(/\w{3}/) // a weekday
    expect(previewLabel({ text: 'x', deadline: '2026-09-11' }, '2026-09-10')).toMatch(/^→ tomorrow · .*11/)
    expect(previewLabel({ text: 'x', deadline: '2026-09-10' }, '2026-09-10')).toMatch(/^→ today · .*10/)
  })
})

describe('summary', () => {
  it('counts only open items', () => {
    expect(openItems([item(), item({ done: true }), item({ carried: true })])).toHaveLength(1)
  })
  it('builds the header line from what is known', () => {
    const now = new Date(2026, 8, 10, 22, 0).getTime()
    const since = new Date(2026, 8, 10, 21, 34).toISOString()
    expect(summaryLine({ todos: 2, inbox: 3, since, now })).toMatch(/^2 to-dos · 3 in Inbox · quiet since \d{1,2}:34/)
    expect(summaryLine({ todos: 1, inbox: 0, since: null })).toBe('1 to-do')
    expect(summaryLine({ todos: 0, inbox: null, since: null })).toBe('no to-dos')
    expect(summaryLine({ todos: null, inbox: null, since: null })).toBe('all clear')
    expect(summaryLine({ todos: null, inbox: null, since: 'garbage' })).toBe('all clear')
  })
})
