// Gardener management (4.1's "form over the registry row"): tweak config,
// toggle, trigger runs — plus the run log. All against the localhost admin API.

import { useCallback, useEffect, useState } from 'react'
import { api, GardenerRun } from './types'

export interface Gardener {
  id: string
  name: string
  kind: 'tagging' | 'reviewer'
  principal: string
  scope_doc: string | null
  task_prompt: string
  schedule: string
  confidence_policy: 'review' | 'gate'
  enabled: boolean
}

export default function Gardeners() {
  const [gardeners, setGardeners] = useState<Gardener[]>([])
  const [runs, setRuns] = useState<GardenerRun[]>([])
  const [running, setRunning] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)

  const load = useCallback(() => {
    api<Gardener[]>('/admin/gardeners').then(setGardeners).catch(console.error)
    api<GardenerRun[]>('/api/runs').then(setRuns).catch(console.error)
  }, [])

  useEffect(load, [load])

  const runNow = async (name: string) => {
    setRunning(name)
    try {
      await api('/admin/garden', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      })
    } catch (e) {
      alert(String(e))
    }
    setRunning(null)
    load()
  }

  return (
    <div className="runs">
      <div className="gardeners-head">
        <h1 className="queue-title">gardeners</h1>
        <button className="chip" onClick={() => setCreating(!creating)}>
          {creating ? 'cancel' : '+ new gardener'}
        </button>
      </div>
      {creating && (
        <CreateCard
          onCreated={() => {
            setCreating(false)
            load()
          }}
        />
      )}
      {gardeners.map((g) => (
        <GardenerCard
          key={g.id}
          g={g}
          running={running === g.name}
          anyRunning={running !== null}
          onRun={() => runNow(g.name)}
          onSaved={load}
        />
      ))}
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
  const dirty =
    prompt !== g.task_prompt ||
    schedule !== g.schedule ||
    policy !== g.confidence_policy ||
    enabled !== g.enabled

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
      }),
    }).catch((e) => alert(String(e)))
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
  const [kind, setKind] = useState<'tagging' | 'reviewer'>('tagging')
  const [prompt, setPrompt] = useState('')

  const create = async () => {
    if (!name.trim() || !prompt.trim()) return
    await api('/admin/gardeners', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: name.trim(), kind, task_prompt: prompt.trim() }),
    }).catch((e) => alert(String(e)))
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
          <select value={kind} onChange={(e) => setKind(e.target.value as 'tagging' | 'reviewer')}>
            <option value="tagging">tagging (sweeps docs)</option>
            <option value="reviewer">reviewer (clears the queue)</option>
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
