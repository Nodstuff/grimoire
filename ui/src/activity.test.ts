import { describe, expect, it } from 'vitest'
import { activityLine, unseenActivity } from './activity'
import { trustHint, trustLabel } from './trust'
import type { ActivityItem } from './types'

function item(op_id: string): ActivityItem {
  return {
    op_id,
    doc_id: 'd',
    doc_title: 'Plan',
    principal: 'remote:x',
    principal_name: 'alice',
    op_type: 'replace',
    epoch: 3,
    created_at: '2026-09-02T10:00:00Z',
  }
}

describe('unseenActivity', () => {
  const feed = [item('c'), item('b'), item('a')] // newest first
  it('first run (no last seen) baselines silently', () => {
    expect(unseenActivity(feed, null)).toEqual([])
  })
  it('returns items newer than the last seen one', () => {
    expect(unseenActivity(feed, 'a').map((i) => i.op_id)).toEqual(['c', 'b'])
    expect(unseenActivity(feed, 'c')).toEqual([])
  })
  it('treats an unknown last seen as everything new', () => {
    expect(unseenActivity(feed, 'zzz').map((i) => i.op_id)).toEqual(['c', 'b', 'a'])
  })
  it('empty feed is empty', () => {
    expect(unseenActivity([], 'a')).toEqual([])
  })
  it('formats the notification line', () => {
    expect(activityLine(item('x'))).toBe('alice edited “Plan”')
  })
})

describe('trust tiers', () => {
  it('labels every tier and defaults unknown to review', () => {
    expect(trustLabel('review')).toBe('review')
    expect(trustLabel('yellow')).toBe('trusted')
    expect(trustLabel('green')).toBe('maintainer')
    expect(trustLabel(undefined)).toBe('review')
    expect(trustHint('green')).toMatch(/notified/)
  })
})
