// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { safeSvg } from './svgSafe'
import { renderReladraw } from './reladraw'

const dom = (html: string) => {
  const d = document.createElement('div')
  d.innerHTML = html
  return d
}

describe('safeSvg', () => {
  it('reladraw `url: "javascript:…"`: the link is dropped, the drawing stays', async () => {
    const r = await renderReladraw('node a "Click me"  url: "javascript:alert(1)"\nnode b "Safe"  right of a  url: "https://example.com/x?a=1&b=2"', 'light')
    if ('err' in r) throw new Error(r.err)
    // reladraw writes the href as given
    expect(r.svg).toContain('javascript:alert(1)')
    const out = dom(safeSvg(r.svg))
    expect(out.innerHTML).not.toMatch(/javascript:/i)
    expect(out.textContent).toContain('Click me')
    const hrefs = [...out.querySelectorAll('a')].map((a) => a.getAttribute('href'))
    expect(hrefs).toContain('https://example.com/x?a=1&b=2')
    for (const a of out.querySelectorAll('a[target="_blank"]')) expect(a.getAttribute('rel')).toMatch(/noopener/)
  })

  it('drops script, handlers, foreign schemes and animation that rewrites links', () => {
    const out = dom(
      safeSvg(
        `<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script>` +
          `<a href="java&#9;script:alert(1)"><text>t</text></a>` +
          `<a xlink:href="data:text/html,x"><rect/></a>` +
          `<g onclick="alert(1)" onmouseover="alert(2)"><rect onload="x()"/></g>` +
          `<a href="#ok"><set attributeName="href" to="javascript:alert(1)"/><animate attributeName="href" values="javascript:alert(1)"/></a>` +
          `<a href="mailto:me@example.com"><text>m</text></a><animateMotion/><animateTransform/></svg>`,
      ),
    )
    const html = out.innerHTML
    expect(html).not.toMatch(/<script|javascript|data:text|onclick|onmouseover|onload|<set|<animate/i)
    expect(out.querySelector('a[href="#ok"]')).not.toBeNull()
    expect(out.querySelector('a[href="mailto:me@example.com"]')).not.toBeNull()
  })

  it("keeps what mermaid draws: style, markers, classes, HTML labels in foreignObject", () => {
    const svg =
      `<svg id="mmd-1" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10" class="flowchart">` +
      `<style>#mmd-1 .node rect{fill:#fff;stroke:#888}</style>` +
      `<defs><marker id="mmd-1_arrow"><path d="M0,0L10,5L0,10z"/></marker></defs>` +
      `<g class="node default" id="flowchart-A-0" transform="translate(5,5)"><rect class="basic label-container" x="-5" y="-5" width="10" height="10" rx="2"/>` +
      `<foreignObject width="40" height="20"><div xmlns="http://www.w3.org/1999/xhtml" style="display: table-cell;"><span class="nodeLabel"><p>Label <b>bold</b></p></span></div></foreignObject></g>` +
      `<path d="M0,0L5,5" class="flowchart-link" marker-end="url(#mmd-1_arrow)"/>` +
      `<text x="1" y="2" font-size="12" fill="#333">plain</text></svg>`
    const out = dom(safeSvg(svg))
    expect(out.querySelector('style')!.textContent).toContain('.node rect')
    expect(out.querySelector('marker#mmd-1_arrow')).not.toBeNull()
    expect(out.querySelector('path.flowchart-link')!.getAttribute('marker-end')).toBe('url(#mmd-1_arrow)')
    expect(out.querySelector('g.node')!.getAttribute('transform')).toBe('translate(5,5)')
    expect(out.querySelector('foreignObject span.nodeLabel')?.textContent).toBe('Label bold')
    expect(out.querySelector('foreignObject div')!.getAttribute('style')).toContain('table-cell')
    expect(out.querySelector('svg')!.getAttribute('viewBox')).toBe('0 0 10 10')
    expect(out.textContent).toContain('plain')
  })
})

describe('drawDiagram', () => {
  it('a reladraw fence with a javascript: url draws without the link (what the preview and share links insert)', async () => {
    const { drawDiagram } = await import('./CodeBlockView')
    const svg = await drawDiagram('reladraw', 'node a "Click me"  url: "javascript:alert(document.cookie)"', false)
    expect(svg).not.toMatch(/javascript:/i)
    expect(svg).toContain('Click me')
  })

  it('d2 from the daemon is sanitised too', async () => {
    const { drawDiagram } = await import('./CodeBlockView')
    const fetchMock = async () =>
      new Response(JSON.stringify({ svg: '<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"><text>d</text></a><script>alert(2)</script></svg>' }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    const real = globalThis.fetch
    globalThis.fetch = fetchMock as typeof fetch
    try {
      const svg = await drawDiagram('d2', 'a -> b', false)
      expect(svg).not.toMatch(/javascript:|<script/i)
      expect(svg).toContain('>d<')
    } finally {
      globalThis.fetch = real
    }
  })
})

describe('safeSvg keeps legitimate content', () => {
  it('real reladraw output: a root link, a pasted icon with <use> and a raster data: image survive; javascript: does not', async () => {
    const src =
      'icon crate """\n<svg viewBox="0 0 10 10"><defs><path id="a" d="M0 0h10v10z"/></defs><use href="#a"/><image href="data:image/png;base64,iVBORw0KGgo=" width="5" height="5"/></svg>\n"""\n' +
      'node c icon: crate\nnode d "Doc" right of c url: "/doc/123"\nnode e "Up" right of d url: "../x"\nnode f "Bad" right of e url: "javascript:alert(1)"\nnode g icon: disk right of f'
    const r = await renderReladraw(src, 'light')
    if ('err' in r) throw new Error(r.err)
    expect(r.svg).toMatch(/<use href="#/)
    const out = dom(safeSvg(r.svg))
    const hrefs = [...out.querySelectorAll('a')].map((a) => a.getAttribute('href'))
    expect(hrefs).toContain('/doc/123')
    expect(hrefs).toContain('../x')
    expect(out.innerHTML).not.toMatch(/javascript:/i)
    const use = out.querySelector('use')!
    expect(use.getAttribute('href')).toMatch(/^#/)
    // (reladraw 0.15.1 prefixes the icon's ids but not its `href="#…"`, so the
    // reference dangles in its own output; the sanitiser keeps both as given)
    expect(out.querySelector('defs path[id]')).not.toBeNull()
    expect(out.querySelector('image')!.getAttribute('href')).toBe('data:image/png;base64,iVBORw0KGgo=')
  })

  it('real mermaid output (jsdom, measured text stubbed): labels, a click link and the arrows survive unchanged', async () => {
    const P = (window as unknown as { SVGElement: { prototype: Record<string, unknown> } }).SVGElement.prototype
    P.getBBox = () => ({ x: 0, y: 0, width: 80, height: 20 })
    P.getComputedTextLength = () => 50
    const mermaid = (await import('mermaid')).default
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict' })
    const { svg } = await mermaid.render('mmd-t1', 'flowchart LR\n  A[Start] -->|go| B(End)\n  click A "/doc/123"')
    const before = dom(svg)
    const out = dom(safeSvg(svg))
    expect(out.querySelector('a')!.getAttribute('href')).toBe('/doc/123')
    expect(out.querySelectorAll('foreignObject').length).toBe(before.querySelectorAll('foreignObject').length)
    expect(out.textContent).toContain('Start')
    expect(out.textContent).toContain('go')
    expect(out.querySelectorAll('marker').length).toBe(before.querySelectorAll('marker').length)
    expect(out.querySelector('style')!.textContent).toBe(before.querySelector('style')!.textContent)
  })

  it('<use> pointing outside the drawing, svg+xml and other schemes still go', () => {
    const out = dom(
      safeSvg(
        '<svg xmlns="http://www.w3.org/2000/svg"><use href="https://evil.example/x.svg#a"/><use href="data:image/svg+xml,<svg/>#a"/>' +
          '<image href="data:image/svg+xml;base64,PHN2Zz48c2NyaXB0PmFsZXJ0KDEpPC9zY3JpcHQ+PC9zdmc+"/><image href="data:text/html,x"/>' +
          '<a href="vbscript:x"><text>v</text></a><a href="ftp://x/y"><text>f</text></a><a href="javascript&#58;alert(1)"><text>j</text></a></svg>',
      ),
    )
    for (const u of out.querySelectorAll('use')) expect(u.getAttribute('href')).toBeNull()
    for (const i of out.querySelectorAll('image')) expect(i.getAttribute('href')).toBeNull()
    for (const a of out.querySelectorAll('a')) expect(a.getAttribute('href')).toBeNull()
  })
})
