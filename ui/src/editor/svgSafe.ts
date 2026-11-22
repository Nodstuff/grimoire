// Diagram SVG is drawn from a doc's text (reladraw and d2 links, mermaid
// labels) and put into the page as markup: whatever a doc says must not run.
// reladraw writes a node's `url:` into `<a href>` unchecked, so a
// `url: "javascript:…"` ran on click in LOCAL mode (no CSP there). Every
// diagram goes through here before it is inserted: DOMPurify (already in the
// bundle, mermaid's own sanitiser) with SVG and HTML-in-foreignObject kept
// for mermaid's labels, then links checked: a link with an explicit scheme
// must be http(s) or mailto (relative and root links, `/doc/123`, `../x`,
// `#a`, pass); a `<use>` may point only inside the drawing (`#…`, reladraw's
// pasted icons); an `<image>` may carry a raster `data:image/…` (not
// svg+xml).

import DOMPurify from 'dompurify'

const SCHEME = /^([a-z][a-z0-9+.-]*):/i
const SAFE_SCHEMES = new Set(['http', 'https', 'mailto'])
const RASTER_DATA = /^data:image\/(?:png|jpe?g|gif|webp|avif|bmp)[;,]/i
const LINK_ATTRS = ['href', 'xlink:href']

/** May this element keep this link? `v` has whitespace and control
 * characters taken out (`java\tscript:` still runs). */
export function linkAllowed(tag: string, v: string): boolean {
  if (tag === 'use') return v.startsWith('#')
  const m = SCHEME.exec(v)
  if (!m) return true // relative, root or a fragment
  const scheme = m[1].toLowerCase()
  if (SAFE_SCHEMES.has(scheme)) return true
  return scheme === 'data' && tag === 'image' && RASTER_DATA.test(v)
}

let purifier: ReturnType<typeof DOMPurify> | null = null
function getPurifier() {
  if (purifier) return purifier
  const p = DOMPurify(window)
  p.addHook('afterSanitizeAttributes', (node) => {
    const tag = node.nodeName.toLowerCase()
    for (const a of LINK_ATTRS) {
      const v = node.getAttribute(a)
      // whitespace and control characters inside a scheme still run
      // (`java\tscript:`): judge the value with them taken out
      if (v != null && !linkAllowed(tag, v.replace(/[\s\u0000-\u001f]+/g, ''))) node.removeAttribute(a)
    }
    if (node.getAttribute('target') === '_blank') node.setAttribute('rel', 'noopener noreferrer')
  })
  purifier = p
  return p
}

/** The diagram's SVG with scripts, event handlers and unsafe links gone. */
export function safeSvg(svg: string): string {
  // mermaid's own settings (mermaid.core: DOMPURIFY_TAGS / _ATTR), so its
  // already-sanitised output passes unchanged, plus the links' `target`.
  // `use` is let in for in-drawing references only (linkAllowed). DOMPurify's
  // defaults already refuse script and the animation elements (`set`,
  // `animate`) that could rewrite an href afterwards; the rest of the SMIL
  // family goes too.
  return getPurifier().sanitize(svg, {
    ADD_TAGS: ['foreignobject', 'use'],
    ADD_ATTR: ['dominant-baseline', 'target'],
    HTML_INTEGRATION_POINTS: { foreignobject: true },
    FORBID_TAGS: ['animatemotion', 'animatetransform'],
  })
}
