// /library[/<workspace>]: every doc in the chosen workspace as a list
// (canvas Library). Stage 1 is the list and the filter; folder pages, tags,
// the Cards view and bulk moves come with the Library screen in stage 2.

import { useMemo, useState } from 'react'
import { Button, Card, Chip, EmptyState, Icon, TextField } from '../lapis'
import Link from '../Link'
import { childrenIndex } from '../tree'
import { docPath, scoreDoc } from '../omni'
import { docsInWorkspace, workspaceColor, workspaceSummary, type Workspace, type WorkspaceSel } from '../workspaces'
import type { Doc } from '../types'

const STATUS_TONE = { decided: 'green', 'in-review': 'yellow', draft: 'neutral', superseded: 'neutral' } as const

export default function Library({
  docs: allDocs,
  workspace,
  workspaces,
  onNewDoc,
}: {
  docs: Doc[]
  workspace: WorkspaceSel
  workspaces: Workspace[]
  onNewDoc: () => void
}) {
  const [filter, setFilter] = useState('')
  const docs = useMemo(() => docsInWorkspace(allDocs, workspace), [allDocs, workspace])
  const byId = useMemo(() => new Map(allDocs.map((d) => [d.id, d])), [allDocs])
  const kids = useMemo(() => childrenIndex(docs), [docs])
  const rows = useMemo(() => {
    const all = docs.map((d) => ({ d, path: docPath(d, byId) }))
    const q = filter.trim()
    if (!q) return all.sort((a, b) => a.path.localeCompare(b.path))
    return all
      .map((r) => ({ ...r, s: scoreDoc(q, r.d.title, r.path) }))
      .filter((r) => r.s !== null)
      .sort((a, b) => (b.s ?? 0) - (a.s ?? 0))
  }, [docs, byId, filter])
  const { name } = workspaceSummary(workspace, workspaces, docs.length)
  const wi = workspaces.findIndex((w) => w.id === workspace)

  return (
    <div className="lp-page lp-library">
      <header className="lp-page-head">
        <div className="lp-page-head-text">
          <div className="lp-page-kicker">
            <Chip dot={wi >= 0 ? workspaceColor(workspaces[wi], wi) : undefined}>{name}</Chip>
            <span className="muted">{docs.length} docs</span>
          </div>
          <h1 className="lp-page-title">Library</h1>
        </div>
        <div className="lp-page-actions">
          <Button size="sm" icon="docPlus" onClick={onNewDoc}>
            New doc
          </Button>
        </div>
      </header>
      <div className="lp-library-tools">
        <TextField label="Filter docs" hideLabel surface="card" icon="search" placeholder="Filter by title or path" value={filter} onChange={(e) => setFilter(e.target.value)} />
      </div>
      {rows.length === 0 ? (
        <Card>
          <EmptyState icon="folder" title={filter ? 'Nothing matches' : 'No docs here yet'} compact>
            {filter ? 'Try fewer letters, or a word from the path.' : 'New docs you file into this workspace show up here.'}
          </EmptyState>
        </Card>
      ) : (
        <Card flush as="section" aria-label={`Docs in ${name}`}>
          <div className="lp-lib-row lp-lib-rowh" aria-hidden>
            <span>Title</span>
            <span>Status</span>
            <span className="lp-lib-n">Inside</span>
          </div>
          {rows.map(({ d, path }) => {
            const n = kids.get(d.id)?.length ?? 0
            return (
              <Link key={d.id} to={{ kind: 'doc', id: d.id }} className="lp-lib-row">
                <span className="lp-lib-title">
                  <Icon name={n > 0 ? 'folder' : 'doc'} size={15} />
                  <span>
                    <b>{d.title}</b>
                    {path !== d.title && <span className="muted lp-lib-path">{path.split(' › ').slice(0, -1).join(' › ')}</span>}
                  </span>
                </span>
                <span>{d.status ? <Chip tag tone={STATUS_TONE[d.status]}>{d.status}</Chip> : <span className="muted">—</span>}</span>
                <span className="lp-lib-n muted">{n > 0 ? n : ''}</span>
              </Link>
            )
          })}
        </Card>
      )}
    </div>
  )
}
