// Code-block NodeView (5.7): a diagram is a block whose content is source —
// mermaid renders client-side, d2 via the daemon; the code stays editable
// and the preview follows it live.

import { NodeViewContent, NodeViewProps, NodeViewWrapper } from '@tiptap/react'
import { useEffect, useRef, useState } from 'react'
import mermaid from 'mermaid'
import { api } from '../types'

mermaid.initialize({
  startOnLoad: false,
  theme: 'dark',
  darkMode: true,
  themeVariables: { background: '#101014', primaryColor: '#1e1e26', lineColor: '#6b6b7b' },
})

let mermaidSeq = 0

function MermaidPreview({ code }: { code: string }) {
  const [svg, setSvg] = useState<string>('')
  const [err, setErr] = useState<string>('')
  useEffect(() => {
    const t = setTimeout(async () => {
      try {
        const { svg } = await mermaid.render(`mmd-${++mermaidSeq}`, code)
        setSvg(svg)
        setErr('')
      } catch (e) {
        setErr(String(e).split('\n')[0])
      }
    }, 400)
    return () => clearTimeout(t)
  }, [code])
  if (err) return <div className="diagram-err">{err}</div>
  return <div className="diagram" dangerouslySetInnerHTML={{ __html: svg }} />
}

function D2Preview({ code }: { code: string }) {
  const [svg, setSvg] = useState<string>('')
  const [err, setErr] = useState<string>('')
  useEffect(() => {
    const t = setTimeout(() => {
      api<{ svg: string }>('/api/render/d2', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ source: code }),
      })
        .then((r) => {
          setSvg(r.svg)
          setErr('')
        })
        .catch((e) => setErr(String(e)))
    }, 600)
    return () => clearTimeout(t)
  }, [code])
  if (err) return <div className="diagram-err">{err}</div>
  return <div className="diagram" dangerouslySetInnerHTML={{ __html: svg }} />
}

export default function CodeBlockView({ node }: NodeViewProps) {
  const lang = (node.attrs.language as string | null) ?? ''
  const code = node.textContent
  const stable = useRef(code)
  stable.current = code
  return (
    <NodeViewWrapper className="codeblock-view">
      <pre data-language={lang}>
        <NodeViewContent />
      </pre>
      {lang === 'mermaid' && code.trim() && <MermaidPreview code={code} />}
      {lang === 'd2' && code.trim() && <D2Preview code={code} />}
    </NodeViewWrapper>
  )
}
