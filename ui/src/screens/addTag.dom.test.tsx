// @vitest-environment jsdom
// "+ tag": Enter adds the tag and unmounts the input, and the input's blur
// then fired done() again — the tag was proposed twice.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { AddTag } from './DocView'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

afterEach(() => {
  document.body.innerHTML = ''
})

async function mount(onAdd: (t: string) => Promise<void>) {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const root = createRoot(el)
  await act(async () => root.render(<AddTag onAdd={onAdd} />))
  await act(async () => el.querySelector('button')!.click())
  const input = el.querySelector('input')!
  return { el, input, root }
}

/** a React-visible change of an input's value */
function setValue(input: HTMLInputElement, v: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, v)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('AddTag', () => {
  it('Enter proposes the tag once, though the input blurs as it goes', async () => {
    const onAdd = vi.fn(async () => {})
    const { input } = await mount(onAdd)
    await act(async () => setValue(input, 'infra'))
    await act(async () => {
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
      // the browser blurs the input as React removes it
      input.dispatchEvent(new FocusEvent('focusout', { bubbles: true }))
    })
    expect(onAdd).toHaveBeenCalledTimes(1)
    expect(onAdd).toHaveBeenCalledWith('infra')
  })

  it('a blur alone adds it once; Escape adds nothing', async () => {
    const onAdd = vi.fn(async () => {})
    const a = await mount(onAdd)
    await act(async () => setValue(a.input, 'x'))
    await act(async () => a.input.dispatchEvent(new FocusEvent('focusout', { bubbles: true })))
    expect(onAdd).toHaveBeenCalledTimes(1)
    // open it again: a second tag works
    await act(async () => a.el.querySelector('button')!.click())
    const again = a.el.querySelector('input')!
    await act(async () => setValue(again, 'y'))
    await act(async () => again.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })))
    expect(onAdd).toHaveBeenCalledTimes(2)
    expect(onAdd).toHaveBeenLastCalledWith('y')
    await act(async () => a.el.querySelector('button')!.click())
    const third = a.el.querySelector('input')!
    await act(async () => setValue(third, 'z'))
    await act(async () => {
      third.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      third.dispatchEvent(new FocusEvent('focusout', { bubbles: true }))
    })
    expect(onAdd).toHaveBeenCalledTimes(2)
  })
})
