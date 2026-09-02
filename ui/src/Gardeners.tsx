// Gardener management (4.1's "form over the registry row"): tweak config,
// toggle, trigger runs — plus the run log. All against the localhost admin API.

import { useCallback, useEffect, useState } from 'react'
import { api, GardenerRun } from './types'
import { notify } from './Notice'

export interface Gardener {
  id: string
  name: string
  kind: 'tagging' | 'reviewer' | 'auditor'
  principal: string
  scope_doc: string | null
  task_prompt: string
  bindings: (string | { path: string })[]
  schedule: string
  confidence_policy: 'review' | 'gate'
  enabled: boolean
}

export default function Gardeners({ dataVersion = 0 }: { dataVersion?: number }) {
  const [gardeners, setGardeners] = useState<Gardener[]>([])
  const [runs, setRuns] = useState<GardenerRun[]>([])
  const [running, setRunning] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  // Claude Code present on this Mac? null = older daemon without the route
  // (assume yes, as before); false hides "+ new gardener" behind an explainer
  const [claude, setClaude] = useState<boolean | null>(null)

  const load = useCallback(() => {
    api<Gardener[]>('/admin/gardeners').then(setGardeners).catch(console.error)
    api<GardenerRun[]>('/api/runs').then(setRuns).catch(console.error)
  }, [])
  useEffect(() => {
    api<{ claude: boolean }>('/api/gardeners/preflight')
      .then((p) => setClaude(p.claude))
      .catch(() => setClaude(null))
  }, [])

  useEffect(load, [load, dataVersion])

  const runNow = async (name: string) => {
    setRunning(name)
    try {
      await api('/admin/garden', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      })
    } catch (e) {
      notify(String(e))
    }
    setRunning(null)
    load()
  }

  return (
    <div className="runs">
      <div className="gardeners-head">
        <h1 className="queue-title">gardeners</h1>
        {claude !== false && (
          <button className="chip" onClick={() => setCreating(!creating)}>
            {creating ? 'cancel' : '+ new gardener'}
          </button>
        )}
      </div>
      {claude === false && (
        <div className="gardeners-empty">
          Gardeners run on Claude Code, which is not installed on this Mac.{' '}
          <a href="https://docs.anthropic.com/en/docs/claude-code" target="_blank" rel="noreferrer">
            Install Claude Code
          </a>
          , then reopen this page.
        </div>
      )}
      {claude !== false && gardeners.length === 0 && !creating && (
        <div className="gardeners-empty">
          gardeners only act on docs you have opted in — open a doc → tend
        </div>
      )}
      {creating && (
        <CreateCard
          onCreated={() => {
            setCreating(false)
            load()
          }}
        />
      )}
      <h2 className="runs-title">global workers</h2>
      {gardeners
        .filter((g) => !g.scope_doc)
        .map((g) => (
          <GardenerCard
            key={g.id}
            g={g}
            running={running === g.name}
            anyRunning={running !== null}
            onRun={() => runNow(g.name)}
            onSaved={load}
          />
        ))}
      {gardeners.some((g) => g.scope_doc) && (
        <>
          <h2 className="runs-title">tended scopes</h2>
          <div className="meta" style={{ marginBottom: 8 }}>
            configured on each doc — open the doc and use its tend panel
          </div>
          {gardeners
            .filter((g) => g.scope_doc)
            .map((g) => (
              <div key={g.id} className={`run ${g.enabled ? '' : 'disabled'}`}>
                <div className="run-head">
                  <span className={`kind-badge kind-${g.kind}`}>{g.kind}</span>
                  <span className="who agent">{g.name}</span>
                  <span className="meta">{g.schedule}</span>
                  {!g.enabled && <span className="meta">disabled</span>}
                </div>
              </div>
            ))}
        </>
      )}
      <h2 className="runs-title">runs</h2>
      {runs.map((r) => (
        <div key={r.id} className="run">
          <div className="run-head">
            <span className="who agent">{r.gardener_name}</span>
            <span className={`status ${r.status}`}>{r.status}</span>
            <span className="meta">{r.started_at.slice(0, 16).replace('T', ' ')}</span>
            {r.tokens_used != null && <span className="meta">{r.tokens_used} tokens</span>}
          </div>
          <pre>{r.summary}</pre>
        </div>
      ))}
      {runs.length === 0 && <div className="empty">no runs yet</div>}
    </div>
  )
}

function GardenerCard({
  g,
  running,
  anyRunning,
  onRun,
  onSaved,
}: {
  g: Gardener
  running: boolean
  anyRunning: boolean
  onRun: () => void
  onSaved: () => void
}) {
  const [prompt, setPrompt] = useState(g.task_prompt)
  const [schedule, setSchedule] = useState(g.schedule)
  const [policy, setPolicy] = useState(g.confidence_policy)
  const [enabled, setEnabled] = useState(g.enabled)
  const initialBindings = (g.bindings ?? [])
    .map((b) => (typeof b === 'string' ? b : b.path))
    .join(', ')
  const [bindings, setBindings] = useState(initialBindings)
  const dirty =
    prompt !== g.task_prompt ||
    schedule !== g.schedule ||
    policy !== g.confidence_policy ||
    enabled !== g.enabled ||
    bindings !== initialBindings

  const save = async () => {
    await api('/admin/gardeners/update', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        id: g.id,
        task_prompt: prompt,
        schedule,
        confidence_policy: policy,
        scope_doc: g.scope_doc,
        enabled,
        bindings: bindings
          .split(',')
          .map((b) => b.trim())
          .filter(Boolean),
      }),
    }).catch((e) => notify(String(e)))
    onSaved()
  }

  return (
    <div className={`card gardener-card ${enabled ? '' : 'disabled'}`}>
      <div className="card-head">
        <span className="who agent">{g.name}</span>
        <span className="meta">{g.kind}</span>
        <span className="card-meta">
          <label className="toggle">
            <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
            enabled
          </label>
        </span>
      </div>
      <textarea
        className="prompt-edit"
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        rows={2}
      />
      {g.kind === 'auditor' && (
        <label className="bindings-label">
          authoritative sources (repo paths, comma-separated) — corrections allowed only when bound
          <input
            className="bindings-input"
            placeholder="/Users/you/code/repo, /another/repo"
            value={bindings}
            onChange={(e) => setBindings(e.target.value)}
          />
        </label>
      )}
      <div className="gardener-row">
        <label>
          schedule
          <input value={schedule} onChange={(e) => setSchedule(e.target.value)} />
        </label>
        <label>
          proposals
          <select value={policy} onChange={(e) => setPolicy(e.target.value as 'review' | 'gate')}>
            <option value="review">always reviewable</option>
            <option value="gate">normal gate verdicts</option>
          </select>
        </label>
        <span className="gardener-actions">
          {dirty && (
            <button className="accept" onClick={save}>
              save
            </button>
          )}
          <button className="chip" disabled={anyRunning || !enabled} onClick={onRun}>
            {running ? 'running…' : 'run now'}
          </button>
        </span>
      </div>
    </div>
  )
}

function CreateCard({ onCreated }: { onCreated: () => void }) {
  const [name, setName] = useState('')
  const [kind, setKind] = useState<'tagging' | 'reviewer' | 'auditor'>('tagging')
  const [prompt, setPrompt] = useState('')

  const create = async () => {
    if (!name.trim() || !prompt.trim()) return
    await api('/admin/gardeners', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: name.trim(), kind, task_prompt: prompt.trim() }),
    }).catch((e) => notify(String(e)))
    onCreated()
  }

  return (
    <div className="card gardener-card">
      <div className="gardener-row">
        <label>
          name
          <input autoFocus value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          kind
          <select value={kind} onChange={(e) => setKind(e.target.value as 'tagging' | 'reviewer' | 'auditor')}>
            <option value="tagging">tagging (sweeps docs)</option>
            <option value="reviewer">reviewer (clears the queue)</option>
            <option value="auditor">auditor (flags stale/suspect claims)</option>
          </select>
        </label>
      </div>
      <textarea
        className="prompt-edit"
        placeholder="task prompt — what should this gardener do?"
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        rows={2}
      />
      <div className="gardener-row">
        <span className="gardener-actions">
          <button className="accept" onClick={create}>
            create
          </button>
        </span>
      </div>
    </div>
  )
}
