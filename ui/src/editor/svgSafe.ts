// Diagram SVG is drawn from a doc's text (reladraw and d2 links, mermaid
// labels) and put into the page as markup: whatever a doc says must not run.
// reladraw writes a node's `url:` into `<a href>` unchecked, so a
// `url: "javascript:…"` ran on click in LOCAL mode (no CSP there). Every
// diagram goes through here before it is inserted: DOMPurify (already in the
// bundle, mermaid's own sanitiser) with SVG and HTML-in-foreignObject kept
// for mermaid's labels, then links narrowed to http(s), mailto and in-drawing
// `#` references.

import DOMPurify from 'dompurify'

const SAFE_LINK = /^(?:https?:|mailto:|#)/i
const LINK_ATTRS = ['href', 'xlink:href']

let purifier: ReturnType<typeof DOMPurify> | null = null
function getPurifier() {
  if (purifier) return purifier
  const p = DOMPurify(window)
  p.addHook('afterSanitizeAttributes', (node) => {
    for (const a of LINK_ATTRS) {
      const v = node.getAttribute(a)
      // whitespace and control characters inside a scheme still run
      // (`java\tscript:`): judge the value with them taken out
      if (v != null && !SAFE_LINK.test(v.replace(/[\s\u0000-\u001f]+/g, ''))) node.removeAttribute(a)
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
  // DOMPurify's defaults already refuse script, `use`, and the animation
  // elements (`set`, `animate`) that could rewrite an href afterwards; the
  // rest of the SMIL family goes too.
  return getPurifier().sanitize(svg, {
    ADD_TAGS: ['foreignobject'],
    ADD_ATTR: ['dominant-baseline', 'target'],
    HTML_INTEGRATION_POINTS: { foreignobject: true },
    FORBID_TAGS: ['animatemotion', 'animatetransform'],
  })
}
