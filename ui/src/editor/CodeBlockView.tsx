// Code-block NodeView (5.7, Lapis Doc artboard): a code block is a card with
// a header (language, Copy); a diagram (mermaid, reladraw, d2) is a figure
// whose header switches Preview / Source / Both. The source is always the
// editable NodeViewContent: Preview only hides it, so the text, the diff and
// the share snapshot (share.ts reads `.codeblock-view pre` + `.diagram svg`)
// never depend on the mode. mermaid and reladraw render client-side, d2 via
// the daemon; the preview follows the code live.

import { NodeViewContent, NodeViewProps, NodeViewWrapper } from '@tiptap/react'
import { useEffect, useState } from 'react'
import { api } from '../types'
import { appTheme, renderReladraw } from './reladraw'
import { DIAGRAM_LANGS } from './diagramLangs'
import { Icon, useTheme } from '../lapis'
import { copyText } from '../Profile'
import { notify } from '../Notice'

// mermaid is ~2MB: fetched on the first mermaid fence, not at boot
let mermaidMod: Promise<typeof import('mermaid')['default']> | null = null
function loadMermaid() {
  if (!mermaidMod) {
    mermaidMod = import('mermaid').then((m) => m.default)
    mermaidMod.catch(() => {
      mermaidMod = null // let the next fence retry the fetch
    })
  }
  return mermaidMod
}

function cssVar(name: string, fallback: string): string {
  try {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
  } catch {
    return fallback
  }
}

/** mermaid drawn in the Lapis palette of the current theme. */
function mermaidConfig(dark: boolean) {
  return {
    startOnLoad: false,
    theme: 'base' as const,
    darkMode: dark,
    fontFamily: cssVar('--font-ui', 'sans-serif'),
    themeVariables: {
      darkMode: dark,
      background: cssVar('--card', dark ? '#181d26' : '#ffffff'),
      fontFamily: cssVar('--font-ui', 'sans-serif'),
      fontSize: '14px',
      primaryColor: cssVar('--card2', dark ? '#1e2430' : '#f6f7fa'),
      primaryTextColor: cssVar('--ink', dark ? '#e8ebf2' : '#141821'),
      primaryBorderColor: cssVar('--ink3', '#8d96a8'),
      secondaryColor: cssVar('--accent-soft', '#e8ebfb'),
      tertiaryColor: cssVar('--card2', '#f6f7fa'),
      lineColor: cssVar('--ink3', '#8d96a8'),
      textColor: cssVar('--ink2', '#454d5e'),
      edgeLabelBackground: cssVar('--card', '#ffffff'),
      clusterBkg: cssVar('--card2', '#f6f7fa'),
      clusterBorder: cssVar('--line2', '#d0d3dc'),
    },
  }
}

let mermaidSeq = 0

function Preview({ lang, code, theme }: { lang: string; code: string; theme: string }) {
  const [svg, setSvg] = useState<string>('')
  const [err, setErr] = useState<string>('')
  useEffect(() => {
    let stale = false
    const t = setTimeout(
      async () => {
        try {
          let out = ''
          if (lang === 'mermaid') {
            const mermaid = await loadMermaid()
            mermaid.initialize(mermaidConfig(theme === 'dark'))
            out = (await mermaid.render(`mmd-${++mermaidSeq}`, code)).svg
          } else if (lang === 'reladraw') {
            const r = await renderReladraw(code, appTheme())
            if ('err' in r) throw new Error(r.err)
            out = r.svg
          } else {
            out = (
              await api<{ svg: string }>('/api/render/d2', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ source: code }),
              })
            ).svg
          }
          if (stale) return
          setSvg(out)
          setErr('')
        } catch (e) {
          if (!stale) setErr(String(e instanceof Error ? e.message : e).split('\n')[0])
        }
      },
      lang === 'd2' ? 600 : 400,
    )
    return () => {
      stale = true
      clearTimeout(t)
    }
  }, [lang, code, theme])
  if (err)
    return (
      <div className="diagram-err" role="alert">
        <Icon name="warning" size={15} /> {err}
      </div>
    )
  if (!svg) return <div className="lazy-loading">Drawing…</div>
  return <div className="diagram" role="img" aria-label={`${lang} diagram`} dangerouslySetInnerHTML={{ __html: svg }} />
}

type Mode = 'preview' | 'source' | 'both'
/** the mode a diagram was left in, per block, for this page's life */
const modes = new Map<string, Mode>()

export default function CodeBlockView({ node }: NodeViewProps) {
  const lang = ((node.attrs.language as string | null) ?? '').trim()
  const lower = lang.toLowerCase()
  const code = node.textContent
  const blockId = (node.attrs.blockId as string | null) ?? null
  const diagram = DIAGRAM_LANGS.has(lower)
  const { theme } = useTheme()
  // an empty diagram has nothing to preview: start on its source
  const [mode, setModeState] = useState<Mode>(() => (blockId && modes.get(blockId)) || (code.trim() ? 'preview' : 'source'))
  const setMode = (m: Mode) => {
    if (blockId) modes.set(blockId, m)
    setModeState(m)
  }
  const showSource = !diagram || mode !== 'preview' || !code.trim()
  const showPreview = diagram && !!code.trim()

  const head = (
    <div className="cb-head" contentEditable={false}>
      <span className="cb-lang">{lang || 'text'}</span>
      {diagram ? (
        <span className="cb-modes" role="group" aria-label="Show">
          {(['preview', 'source', 'both'] as Mode[]).map((m) => (
            <button
              key={m}
              type="button"
              className={`cb-mode${mode === m ? ' on' : ''}`}
              aria-pressed={mode === m}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => setMode(m)}
            >
              {m === 'preview' ? 'Preview' : m === 'source' ? 'Source' : 'Both'}
            </button>
          ))}
        </span>
      ) : (
        <button
          type="button"
          className="lp-btn lp-btn-ghost lp-btn-sm cb-copy"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() =>
            copyText(code)
              .then(() => notify('copied', 'ok'))
              .catch(() => notify('could not copy', 'warn'))
          }
        >
          <Icon name="copy" size={15} />
          Copy
        </button>
      )}
    </div>
  )

  return (
    <NodeViewWrapper className={`codeblock-view ${diagram ? 'cb-fig' : 'cb-code'} cb-${showSource ? 'src' : 'nosrc'}`}>
      {head}
      {showPreview && (
        // kept mounted in Source mode (hidden) so a share snapshot still
        // finds the drawn SVG
        <div className="cb-preview" hidden={mode === 'source'} contentEditable={false}>
          <Preview lang={lower} code={code} theme={theme} />
        </div>
      )}
      <pre data-language={lang} hidden={!showSource} spellCheck={false}>
        <NodeViewContent />
      </pre>
    </NodeViewWrapper>
  )
}
