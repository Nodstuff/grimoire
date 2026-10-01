import { beforeEach, describe, expect, it, vi } from 'vitest'

// a plain function, not vi.fn: vitest reports a throw from a spy as the test's failure
const calls: unknown[][] = []
let impl: (...a: unknown[]) => string = () => '<svg/>'

vi.mock('reladraw', () => {
  class SourceError extends Error {
    constructor(
      message: string,
      readonly line: number,
    ) {
      super(message)
    }
    format() {
      return `line ${this.line}: ${this.message}`
    }
  }
  return {
    compile: (...a: unknown[]) => {
      calls.push(a)
      return impl(...a)
    },
    SourceError,
    THEMES: { dark: { name: 'dark' }, light: { name: 'light' } },
  }
})

const { renderReladraw } = await import('./reladraw')
const { SourceError } = (await import('reladraw')) as unknown as {
  SourceError: new (m: string, l: number) => Error
}

describe('renderReladraw', () => {
  beforeEach(() => {
    calls.length = 0
    impl = () => '<svg/>'
  })

  it('compiles the source with the app theme', async () => {
    expect(await renderReladraw('node a', 'light')).toEqual({ svg: '<svg/>' })
    expect(calls[0]).toEqual(['node a', { theme: { name: 'light' } }])
    await renderReladraw('node a', 'dark')
    expect(calls[1]).toEqual(['node a', { theme: { name: 'dark' } }])
  })

  it("leaves a file's own theme alone", async () => {
    const src = 'diagram theme: nord\nnode a'
    await renderReladraw(src, 'dark')
    expect(calls[0]).toEqual([src])
  })

  it("reports a source error with reladraw's message and line", async () => {
    impl = () => {
      throw new SourceError('unknown statement "a"', 3)
    }
    expect(await renderReladraw('node x\n\na', 'dark')).toEqual({ err: 'line 3: unknown statement "a"' })
  })

  it('reports any other failure by its first line', async () => {
    impl = () => {
      throw new Error('boom\nstack')
    }
    expect(await renderReladraw('node a', 'dark')).toEqual({ err: 'Error: boom' })
  })
})
