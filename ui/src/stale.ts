// Doc freshness + living answers: the pure label logic behind the header
// chips and the Stale docs list. Kept out of the components so it tests
// with a fixed `now`.

import { relTime } from './time'

export interface FreshnessRow {
  id: string
  title: string
  path: string
  verified_at: string | null
  last_edited: string
  tended: boolean
}

export interface LivingSource {
  block_id: string
  doc_title: string | null
  changed: boolean
  gone: boolean
  epoch_at_answer: number
  current_epoch: number | null
  recorded_at: string
}

export interface LivingStatus {
  is_answer: boolean
  question: string | null
  sources: LivingSource[]
  changed: number
  last_refreshed: string | null
}

/** `verified 3d ago` / `never verified`. */
export function verifiedLabel(verifiedAt: string | null | undefined, now: number = Date.now()): string {
  if (!verifiedAt) return 'never verified'
  return `verified ${relTime(verifiedAt, now)}`
}

/** `answer · verified 2h ago` while every cited block stands; `answer · 2
 * sources changed — refreshing` once any moved on. Null for a non-answer. */
export function livingLabel(status: LivingStatus | null | undefined, now: number = Date.now()): string | null {
  if (!status || !status.is_answer) return null
  if (status.changed > 0) {
    return `answer · ${status.changed} source${status.changed === 1 ? '' : 's'} changed — refreshing`
  }
  const when = status.last_refreshed ? relTime(status.last_refreshed, now) : 'just now'
  return `answer · verified ${when}`
}

/** The chip's tone: grey until verified, yellow while a refresh is due. */
export function livingTone(status: LivingStatus | null | undefined): 'fresh' | 'stale' | null {
  if (!status || !status.is_answer) return null
  return status.changed > 0 ? 'stale' : 'fresh'
}
