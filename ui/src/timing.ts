// Small timer helpers shared by the autosave paths. Both are plain closures
// (no React) so they can be unit-tested with fake timers and kept in refs.

export interface Debounced {
  /** (re)start the wait: the call lands `ms` after the LAST arm */
  arm: () => void
  /** drop a pending call */
  cancel: () => void
  /** run now if a call is pending (unmount, ⌘↵) */
  flush: () => void
  pending: () => boolean
}

/** Trailing debounce: `fn` runs once, `ms` after the most recent `arm()`.
 * Every keystroke re-arms, so a typist who never pauses never saves until
 * they do — the point of a debounce, and what an effect keyed on
 * `editor.state.doc` failed to do under Tiptap 3 (it does not re-render per
 * transaction). */
export function debounce(ms: number, fn: () => void): Debounced {
  let t: ReturnType<typeof setTimeout> | null = null
  const cancel = () => {
    if (t) clearTimeout(t)
    t = null
  }
  return {
    arm: () => {
      cancel()
      t = setTimeout(() => {
        t = null
        fn()
      }, ms)
    },
    cancel,
    flush: () => {
      if (!t) return
      cancel()
      fn()
    },
    pending: () => t !== null,
  }
}

export interface Throttled {
  /** request a run: immediate if idle, otherwise coalesced into one trailing call */
  call: () => void
  /** run the trailing call now if one is queued */
  flush: () => void
  cancel: () => void
}

/** Leading + trailing throttle: the first call runs at once, further calls
 * inside the window collapse into ONE call at the window's end. A drag at
 * 60fps becomes ~16 pushes a second instead of 60. */
export function throttle(ms: number, fn: () => void): Throttled {
  let t: ReturnType<typeof setTimeout> | null = null
  let trailing = false
  const fire = () => {
    trailing = false
    fn()
    t = setTimeout(() => {
      t = null
      if (trailing) fire()
    }, ms)
  }
  return {
    call: () => {
      if (t) {
        trailing = true
        return
      }
      fire()
    },
    flush: () => {
      if (t && trailing) {
        clearTimeout(t)
        t = null
        trailing = false
        fn()
      }
    },
    cancel: () => {
      if (t) clearTimeout(t)
      t = null
      trailing = false
    },
  }
}
