// Gardeners (no artboard; built in the Lapis page frame): the scheduled
// agents that tag, audit and file docs, and what they did. Workers are
// cards of rows (agent mark + mono name, kind, schedule, how their changes
// land, on/off); a row opens its settings in place (LOCAL mode, with the
// admin token: prompt, schedule, review policy, bindings, Save, Run now).
// Tended scopes are configured on each doc. Runs are a flush card of rows:
// status pill, verdict chips, tokens, when, the per-item log behind
// "Details". On the server, and in a browser tab without the admin token,
// /admin is refused: the page is read-only and says where to manage them.

import { useCallback, useEffect, useMemo, useState } from 'react'
import { api, ApiError, GardenerRun } from './types'
import { errText, notify } from './Notice'
import { copyText } from './Profile'
import { relTime } from './time'
import { fmtElapsed, fmtTokens, parseProgress, parseSummary, runElapsed, runTone } from './runs'
import { failureLine, runChips } from './RunStatus'
import { AgentGlyph, Button, Card, Chip, EmptyState, Icon, SkeletonLines, Spinner, TextArea } from './lapis'
import './screens/places.css'

// target=_blank / window.open are inert in Tauri's webview: show the URL
const CLAUDE_CODE_URL = 'https://docs.anthropic.com/en/docs/claude-code'

export interface Gardener {
  id: string
  name: string
  /** 'reviewer' only on old, disabled rows: the kind was retired */
  kind: 'tagging' | 'reviewer' | 'auditor' | 'filer'
  principal: string
  scope_doc: string | null
  task_prompt: string
  bindings: (string | { path: string })[]
  schedule: string
  confidence_policy: 'review' | 'gate'
  enabled: boolean
}

const KIND_WORD: Record<Gardener['kind'], string> = { tagging: 'tags', auditor: 'audits', filer: 'files the Inbox', reviewer: 'reviewer (retired)' }
const POLICY_WORD = { review: 'Every change waits for review', gate: 'Normal gate verdicts' } as const

/** Why management is off: null = it is on. */
type Locked = null | { why: 'server' | 'token' | 'error'; detail?: string }

export default function Gardeners({ dataVersion = 0, server = false }: { dataVersion?: number; server?: boolean }) {
  const [gardeners, setGardeners] = useState<Gardener[] | null>(null)
  const [runs, setRuns] = useState<GardenerRun[] | null>(null)
  const [locked, setLocked] = useState<Locked>(null)
  const [running, setRunning] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [openId, setOpenId] = useState<string | null>(null)
  // Claude Code present where Taisce runs? null = older daemon without the
  // route (assume yes, as before); false hides "New gardener" behind an explainer
  const [claude, setClaude] = useState<boolean | null>(null)

  const load = useCallback(() => {
    api<Gardener[]>('/admin/gardeners')
      .then((g) => {
        setGardeners(Array.isArray(g) ? g : [])
        setLocked(null)
      })
      .catch((e) => {
        setGardeners(null)
        setLocked(server ? { why: 'server' } : e instanceof ApiError && e.code === 'admin_token' ? { why: 'token' } : { why: 'error', detail: errText(e) })
      })
    api<GardenerRun[]>('/api/runs')
      .then((r) => setRuns(Array.isArray(r) ? r : []))
      .catch(() => setRuns([]))
  }, [server])
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
      notify(errText(e))
    }
    setRunning(null)
    load()
  }

  const workers = (gardeners ?? []).filter((g) => !g.scope_doc)
  const tended = (gardeners ?? []).filter((g) => g.scope_doc)
  // read-only: the workers we can see are the ones that ran
  const seen = useMemo(() => {
    const m = new Map<string, GardenerRun>()
    for (const r of runs ?? []) if (!m.has(r.gardener_name)) m.set(r.gardener_name, r)
    return [...m.values()]
  }, [runs])
  const kindOf = (r: GardenerRun) => gardeners?.find((g) => g.id === r.gardener)?.kind
  const manage = locked === null && gardeners !== null
  const live = (runs ?? []).filter((r) => r.status === 'running').length

  return (
    <div className="lp-page lp-place lp-gard">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker">
            <span className="lp-pill">
              <Icon name="gardeners" size={13} />
              Gardeners
            </span>
            <span className="muted">
              {manage
                ? `${workers.length} worker${workers.length === 1 ? '' : 's'} · ${tended.length} tended scope${tended.length === 1 ? '' : 's'}`
                : `${seen.length} seen in recent runs`}
              {live > 0 ? ` · ${live} running now` : ''}
            </span>
          </div>
          <h1 className="lp-page-title">Gardeners</h1>
          <p className="lp-page-lead">Scheduled agents that tag, audit and file your docs. Their changes pass the review gate like anyone else’s.</p>
        </div>
        {manage && claude !== false && (
          <div className="lp-page-actions">
            <Button size="sm" variant={creating ? 'secondary' : 'primary'} icon={creating ? 'close' : 'plus'} onClick={() => setCreating((c) => !c)}>
              {creating ? 'Cancel' : 'New gardener'}
            </Button>
          </div>
        )}
      </header>

      {locked && (
        <Card className="lp-gard-note" role="status">
          <Icon name="info" size={18} />
          <div>
            <b>{locked.why === 'server' ? 'Read only on the server' : locked.why === 'token' ? 'Read only in this tab' : 'Could not load the gardeners'}</b>
            <p>
              {locked.why === 'server'
                ? 'Gardeners are set up and run from the Taisce app on your Mac. Here you see what they did.'
                : locked.why === 'token'
                  ? 'Managing gardeners needs the app’s admin token. Open Taisce from the app to change them; their runs are below.'
                  : locked.detail}
            </p>
          </div>
        </Card>
      )}

      {manage && claude === false && (
        <Card className="lp-gard-note">
          <Icon name="warning" size={18} />
          <div>
            <b>Claude Code is not installed where Taisce runs</b>
            <p>
              Gardeners run on it. Install it from <span className="mono">{CLAUDE_CODE_URL}</span>, then reopen this page.
            </p>
          </div>
          <Button
            size="sm"
            icon="copy"
            onClick={() =>
              copyText(CLAUDE_CODE_URL)
                .then(() => notify('link copied', 'ok'))
                .catch(() => notify('could not copy — select the link and copy it by hand', 'warn'))
            }
          >
            Copy link
          </Button>
        </Card>
      )}

      {creating && (
        <CreateCard
          onCreated={() => {
            setCreating(false)
            load()
          }}
        />
      )}

      {/* ---- workers ---- */}
      <section className="lp-place-sec" aria-labelledby="gard-workers">
        <h2 id="gard-workers" className="lp-place-h2">
          Workers
        </h2>
        {gardeners === null && runs === null ? (
          <Card>
            <SkeletonLines lines={3} />
          </Card>
        ) : manage ? (
          workers.length === 0 ? (
            <Card>
              <EmptyState icon="gardeners" title="No workers yet" compact>
                A worker sweeps every doc on a schedule. To have agents look after one doc, open it and use Tend.
              </EmptyState>
            </Card>
          ) : (
            <section className="lp-card lp-card-flush" aria-label="Workers">
              {workers.map((g) => (
                <WorkerRow
                  key={g.id}
                  g={g}
                  open={openId === g.id}
                  onToggle={() => setOpenId(openId === g.id ? null : g.id)}
                  running={running === g.name}
                  anyRunning={running !== null}
                  onRun={() => runNow(g.name)}
                  onSaved={load}
                />
              ))}
            </section>
          )
        ) : seen.length === 0 ? (
          <Card>
            <EmptyState icon="gardeners" title="No gardener has run yet" compact>
              Their runs show up here once they start.
            </EmptyState>
          </Card>
        ) : (
          <section className="lp-card lp-card-flush" aria-label="Workers seen in runs">
            {seen.map((r) => (
              <div key={r.gardener_name} className="lp-gard-row">
                <span className="lp-gard-who">
                  <span className="lp-gard-bot" aria-hidden>
                    <AgentGlyph size={13} />
                  </span>
                  <span className="lp-gard-name">{r.gardener_name}</span>
                </span>
                <span className="muted lp-gard-meta">last ran {relTime(r.started_at)}</span>
                <RunPill run={r} />
              </div>
            ))}
          </section>
        )}
      </section>

      {/* ---- tended scopes ---- */}
      {manage && tended.length > 0 && (
        <section className="lp-place-sec" aria-labelledby="gard-tended">
          <h2 id="gard-tended" className="lp-place-h2">
            Tended scopes <span className="muted lp-place-h2-note">configured on each doc: open it and use Tend</span>
          </h2>
          <section className="lp-card lp-card-flush" aria-label="Tended scopes">
            {tended.map((g) => (
              <div key={g.id} className={`lp-gard-row${g.enabled ? '' : ' off'}`}>
                <span className="lp-gard-who">
                  <span className="lp-gard-bot" aria-hidden>
                    <AgentGlyph size={13} />
                  </span>
                  <span className="lp-gard-name">{g.name}</span>
                </span>
                <span className="lp-gard-meta">
                  <Chip tag>{KIND_WORD[g.kind] ?? g.kind}</Chip>
                  <span className="muted mono">{g.schedule}</span>
                </span>
                <span className={`lp-gard-state${g.enabled ? ' on' : ''}`}>{g.enabled ? 'On' : 'Off'}</span>
              </div>
            ))}
          </section>
        </section>
      )}

      {/* ---- runs ---- */}
      <section className="lp-place-sec" aria-labelledby="gard-runs">
        <h2 id="gard-runs" className="lp-place-h2">
          Runs
        </h2>
        {runs === null ? (
          <Card>
            <SkeletonLines lines={3} />
          </Card>
        ) : runs.length === 0 ? (
          <Card>
            <EmptyState icon="history" title="No runs yet" compact>
              Each run lists what it changed, with the gate’s verdicts.
            </EmptyState>
          </Card>
        ) : (
          <section className="lp-card lp-card-flush" aria-label="Recent runs">
            {runs.map((r) => (
              <RunRow key={r.id} run={r} kind={kindOf(r)} />
            ))}
          </section>
        )}
      </section>
    </div>
  )
}

/** ok / failed / budget-killed / running, as colour + dot + word. */
function RunPill({ run, now }: { run: GardenerRun; now?: number }) {
  const tone = runTone(run.status)
  const label = tone === 'running' ? `Running ${fmtElapsed(runElapsed(run, now))}` : run.status === 'ok' ? 'Done' : run.status === 'failed' ? 'Failed' : run.status.replace('-', ' ')
  return (
    <span className={`lp-runpill ${tone}`} title={run.status}>
      {tone === 'running' ? <Spinner size={11} label="running" /> : <i aria-hidden />}
      {label}
    </span>
  )
}

/** One run: who, kind, status, verdict chips, tokens, when; the per-item log
 * behind Details; a running run's live line; a failed run's error. */
function RunRow({ run: r, kind }: { run: GardenerRun; kind?: Gardener['kind'] }) {
  const [open, setOpen] = useState(false)
  const summary = parseSummary(r.summary)
  const running = r.status === 'running'
  const fail = failureLine(r)
  const chips = runChips(r)
  const progress = running ? parseProgress(r.summary) : null
  const hasDetails = summary.lines.length > 0 || (!running && !fail && !!summary.headline && chips.length === 0)
  return (
    <div className={`lp-run${open ? ' open' : ''}`}>
      <div className="lp-run-head">
        <span className="lp-gard-who">
          <span className="lp-gard-bot" aria-hidden>
            <AgentGlyph size={13} />
          </span>
          <span className="lp-gard-name">{r.gardener_name}</span>
          {kind && <Chip tag>{KIND_WORD[kind] ?? kind}</Chip>}
        </span>
        <RunPill run={r} />
        {chips.length > 0 && (
          <span className="lp-run-chips">
            {chips.map((c, i) => (
              <Chip key={`${c.text}-${i}`} tone={c.tone ?? 'neutral'}>
                {c.text}
              </Chip>
            ))}
          </span>
        )}
        <span className="lp-run-end">
          {r.tokens_used != null && r.tokens_used > 0 && (
            <span className="muted" title={`${r.tokens_used} tokens`}>
              {fmtTokens(r.tokens_used)} tok
            </span>
          )}
          <span className="muted" title={new Date(r.started_at).toLocaleString()}>
            {relTime(r.started_at)}
          </span>
          {hasDetails && (
            <Button size="sm" variant="ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
              {open ? 'Hide' : 'Details'}
            </Button>
          )}
        </span>
      </div>
      {running && (
        <div className="lp-run-line muted">
          working… {progress ? `${fmtElapsed(progress.seconds)} · ${progress.toolCalls} tool call${progress.toolCalls === 1 ? '' : 's'}${progress.last ? ` · last: ${progress.last}` : ''}` : ''}
        </div>
      )}
      {fail && <div className="lp-run-line lp-run-fail">{fail}</div>}
      {!running && !fail && summary.headline && chips.length > 0 && <div className="lp-run-line muted">{summary.headline}</div>}
      {open && (
        <div className="lp-run-log">
          {summary.headline && chips.length === 0 && <div>{summary.headline}</div>}
          {summary.lines.map((l, i) => (
            <div key={i}>{l}</div>
          ))}
        </div>
      )}
    </div>
  )
}

/** A worker's row; open, its settings in place. */
function WorkerRow({
  g,
  open,
  onToggle,
  running,
  anyRunning,
  onRun,
  onSaved,
}: {
  g: Gardener
  open: boolean
  onToggle: () => void
  running: boolean
  anyRunning: boolean
  onRun: () => void
  onSaved: () => void
}) {
  const [prompt, setPrompt] = useState(g.task_prompt)
  const [schedule, setSchedule] = useState(g.schedule)
  const [policy, setPolicy] = useState(g.confidence_policy)
  const [enabled, setEnabled] = useState(g.enabled)
  const initialBindings = (g.bindings ?? []).map((b) => (typeof b === 'string' ? b : b.path)).join(', ')
  const [bindings, setBindings] = useState(initialBindings)
  const [saving, setSaving] = useState(false)
  const dirty = prompt !== g.task_prompt || schedule !== g.schedule || policy !== g.confidence_policy || enabled !== g.enabled || bindings !== initialBindings

  const save = async () => {
    setSaving(true)
    try {
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
      })
      notify(`saved ${g.name}`, 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setSaving(false)
    onSaved()
  }

  const panel = `gard-${g.id}`
  return (
    <div className={`lp-gard-worker${open ? ' open' : ''}${g.enabled ? '' : ' off'}`}>
      <button type="button" className="lp-gard-row lp-gard-rowbtn" aria-expanded={open} aria-controls={panel} onClick={onToggle}>
        <span className="lp-gard-who">
          <span className="lp-gard-bot" aria-hidden>
            <AgentGlyph size={13} />
          </span>
          <span className="lp-gard-name">{g.name}</span>
        </span>
        <span className="lp-gard-meta">
          <Chip tag>{KIND_WORD[g.kind] ?? g.kind}</Chip>
          <span className="muted mono">{g.schedule}</span>
          <span className="muted lp-gard-policy">{POLICY_WORD[g.confidence_policy]}</span>
        </span>
        <span className={`lp-gard-state${g.enabled ? ' on' : ''}`}>{g.enabled ? 'On' : 'Off'}</span>
        <Icon name={open ? 'chevronUp' : 'chevronDown'} size={15} className="lp-gard-chev" />
      </button>
      {open && (
        <div className="lp-gard-edit" id={panel}>
          <TextArea label="What it does (its task prompt)" value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={3} />
          {g.kind === 'auditor' && (
            <label className="lp-gard-field wide">
              <span>Authoritative sources (repo paths, comma-separated): corrections only when bound</span>
              <input placeholder="/Users/you/code/repo, /another/repo" value={bindings} onChange={(e) => setBindings(e.target.value)} />
            </label>
          )}
          <div className="lp-gard-fields">
            <label className="lp-gard-field">
              <span>Schedule</span>
              <input className="mono" value={schedule} onChange={(e) => setSchedule(e.target.value)} />
            </label>
            <label className="lp-gard-field">
              <span>Its changes</span>
              <select value={policy} onChange={(e) => setPolicy(e.target.value as 'review' | 'gate')}>
                <option value="review">always wait for review</option>
                <option value="gate">get normal gate verdicts</option>
              </select>
            </label>
            <label className="lp-gard-toggle">
              <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
              <span>On</span>
            </label>
            <span className="lp-gard-actions">
              <Button size="sm" icon="gardeners" disabled={anyRunning || !g.enabled} onClick={onRun}>
                {running ? 'Running…' : 'Run now'}
              </Button>
              <Button size="sm" variant="primary" disabled={!dirty || saving} onClick={save}>
                {saving ? 'Saving…' : 'Save'}
              </Button>
            </span>
          </div>
        </div>
      )}
    </div>
  )
}

function CreateCard({ onCreated }: { onCreated: () => void }) {
  const [name, setName] = useState('')
  const [kind, setKind] = useState<'tagging' | 'auditor' | 'filer'>('tagging')
  const [prompt, setPrompt] = useState('')
  const [busy, setBusy] = useState(false)

  const create = async () => {
    if (!name.trim() || !prompt.trim() || busy) return
    setBusy(true)
    try {
      await api('/admin/gardeners', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: name.trim(), kind, task_prompt: prompt.trim() }),
      })
      notify(`made the gardener ${name.trim()}`, 'ok')
      onCreated()
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }

  return (
    <Card className="lp-gard-create">
      <h2 className="lp-place-h2">New gardener</h2>
      <div className="lp-gard-fields">
        <label className="lp-gard-field">
          <span>Name</span>
          <input autoFocus className="mono" placeholder="e.g. tagging" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="lp-gard-field">
          <span>Kind</span>
          <select value={kind} onChange={(e) => setKind(e.target.value as 'tagging' | 'auditor' | 'filer')}>
            <option value="tagging">tagging (sweeps docs)</option>
            <option value="auditor">auditor (flags stale or suspect claims)</option>
            <option value="filer">filer (empties the Inbox: folder, title, tags)</option>
          </select>
        </label>
      </div>
      <TextArea label="What should it do?" placeholder="Its task prompt" value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={3} />
      <div className="lp-gard-actions">
        <Button variant="primary" size="sm" disabled={!name.trim() || !prompt.trim() || busy} onClick={create}>
          {busy ? 'Creating…' : 'Create'}
        </Button>
      </div>
    </Card>
  )
}
