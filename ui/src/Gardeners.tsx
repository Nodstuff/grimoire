// Gardener management (4.1's "form over the registry row"): tweak config,
// toggle, trigger runs — plus the run log. All against the localhost admin API.

import { useCallback, useEffect, useState } from 'react'
import { api, GardenerRun } from './types'
import { notify } from './Notice'
import { copyText } from './Profile'
import { relTime } from './time'
import { fmtTokens, parseSummary } from './runs'
import { Chips, ProgressLine, StatusPill, failureLine, runChips } from './RunStatus'

// target=_blank / window.open are inert in Tauri's webview: show the URL
const CLAUDE_CODE_URL = 'https://docs.anthropic.com/en/docs/claude-code'

export interface Gardener {
  id: string
  name: string
  kind: 'tagging' | 'reviewer' | 'auditor' | 'filer'
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
          Gardeners run on Claude Code, which is not installed on this Mac. Install it from{' '}
          <span className="mono">{CLAUDE_CODE_URL}</span>{' '}
          <button
            className="chip"
            onClick={() =>
              copyText(CLAUDE_CODE_URL)
                .then(() => notify('link copied', 'ok'))
                .catch(() => notify('could not copy — select the link and copy it by hand', 'warn'))
            }
          >
            copy link
          </button>
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
      <div className="run-list">
        {runs.map((r) => (
          <RunRow key={r.id} run={r} kind={gardeners.find((g) => g.id === r.gardener)?.kind} />
        ))}
      </div>
      {runs.length === 0 && <div className="empty">no runs yet</div>}
    </div>
  )
}

/** One run as a structured row: kind badge, name, status pill, when, tokens,
 * the headline chips; the per-item log behind a disclosure; a running run's
 * live line; a failed run's error in the row itself. */
function RunRow({ run: r, kind }: { run: GardenerRun; kind?: Gardener['kind'] }) {
  const [open, setOpen] = useState(false)
  const summary = parseSummary(r.summary)
  const running = r.status === 'running'
  const fail = failureLine(r)
  const chips = runChips(r)
  const hasDetails = summary.lines.length > 0 || (!running && !fail && !!summary.headline && chips.length === 0)
  return (
    <div className={`run-row status-${r.status} ${open ? 'open' : ''}`}>
      <div className="run-head">
        {kind && <span className={`kind-badge kind-${kind}`}>{kind}</span>}
        <span className="who agent">{r.gardener_name}</span>
        <StatusPill run={r} />
        <Chips chips={chips} />
        <span className="run-spacer" />
        {r.tokens_used != null && r.tokens_used > 0 && (
          <span className="meta run-tokens" title={`${r.tokens_used} tokens`}>
            {fmtTokens(r.tokens_used)} tok
          </span>
        )}
        <span className="meta run-when" title={new Date(r.started_at).toLocaleString()}>
          {relTime(r.started_at)}
        </span>
        {hasDetails && (
          <button className="run-details-btn" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
            {open ? 'hide' : 'details'}
          </button>
        )}
      </div>
      {running && <ProgressLine run={r} />}
      {fail && <div className="run-failure">{fail}</div>}
      {!running && !fail && summary.headline && chips.length > 0 && <div className="run-note">{summary.headline}</div>}
      {open && (
        <div className="run-log">
          {summary.headline && chips.length === 0 && <div className="run-log-line">{summary.headline}</div>}
          {summary.lines.map((l, i) => (
            <div key={i} className="run-log-line">
              {l}
            </div>
          ))}
        </div>
      )}
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
  const [kind, setKind] = useState<'tagging' | 'reviewer' | 'auditor' | 'filer'>('tagging')
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
          <select value={kind} onChange={(e) => setKind(e.target.value as 'tagging' | 'reviewer' | 'auditor' | 'filer')}>
            <option value="tagging">tagging (sweeps docs)</option>
            <option value="reviewer">reviewer (clears the queue)</option>
            <option value="auditor">auditor (flags stale/suspect claims)</option>
            <option value="filer">filer (empties the Inbox: folder, title, tags)</option>
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
