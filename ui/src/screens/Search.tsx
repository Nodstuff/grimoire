// /search?q=…: Search & Ask as a page. Stage 1 hosts the ⌘⇧F palette's body
// inline (same hits, same ask); the Search artboard's layout is stage 2.

import SearchPalette from '../SearchPalette'
import { navigate } from '../router'
import type { OpenDoc } from '../App'

export default function Search({ q, onOpenDoc }: { q: string; onOpenDoc: OpenDoc }) {
  return (
    <div className="lp-page lp-search-page">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <h1 className="lp-page-title">Search &amp; Ask</h1>
          <p className="lp-page-lead">Full text across every doc, typos fine. Start with ? to ask the library a question.</p>
        </div>
      </header>
      <SearchPalette
        inline
        initial={q}
        onOpenDoc={onOpenDoc}
        onClose={() => navigate({ kind: 'home' })}
        onQuery={(v) => navigate({ kind: 'search', q: v }, { replace: true })}
      />
    </div>
  )
}
