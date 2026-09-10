// The ⌘K command list, as data: what the omnibox's Commands group ranks and
// runs. Moved out of the old CommandPalette so the list is one place and the
// omnibox stays a mixer. Context-dependent rows (per-doc export, the native
// Save sheet) are included only when they apply.

import { notify, errText } from './Notice'
import { copyText } from './Profile'
import { backupFileName, inTauri, saveDialog } from './tauri'
import { api } from './types'
import type { OmniCommand } from './omni'

export type CommandAction =
  | 'review'
  | 'runs'
  | 'tree'
  | 'home'
  | 'newdoc'
  | 'newcanvas'
  | 'graph'
  | 'sharing'
  | 'profile'
  | 'trash'
  | 'freshness'
  | 'capture'
  | 'close'

export interface CommandCtx {
  queueCount: number
  /** the open doc, if any — enables the per-doc commands */
  docId: string | null
  /** the root doc titled Inbox, when quick capture has made one */
  inboxId: string | null
  onAction: (a: CommandAction) => void
  onOpenDoc: (id: string) => void
}

export function buildCommands({ queueCount, docId, inboxId, onAction, onOpenDoc }: CommandCtx): OmniCommand[] {
  const cmds: OmniCommand[] = [
    { id: 'review', label: 'Review queue', hint: queueCount ? `${queueCount} open` : undefined, keys: '⌘⇧R', run: () => onAction('review') },
    {
      id: 'inbox',
      label: 'Inbox',
      hint: 'captured notes, waiting to be filed',
      run: () => {
        onAction('close')
        if (inboxId) onOpenDoc(inboxId)
        else notify('nothing captured yet — ⌘⇧I captures a note', 'ok')
      },
    },
    { id: 'capture', label: 'Quick capture…', hint: 'a note straight into Inbox', keys: '⌘⇧I', run: () => onAction('capture') },
    { id: 'newdoc', label: 'New doc…', keys: '⌘N', run: () => onAction('newdoc') },
    { id: 'newcanvas', label: 'New canvas…', keys: '⌘⇧N', run: () => onAction('newcanvas') },
    { id: 'home', label: 'Home', hint: 'the briefing', keys: '⌘W', run: () => onAction('home') },
    { id: 'runs', label: 'Gardeners', keys: '⌘G', run: () => onAction('runs') },
    { id: 'sharing', label: 'Shares & contacts', run: () => onAction('sharing') },
    { id: 'profile', label: 'Profile', hint: 'your name, node id, fingerprint', run: () => onAction('profile') },
    { id: 'graph', label: 'Graph view', run: () => onAction('graph') },
    { id: 'trash', label: 'Trash', hint: 'restore deleted docs', run: () => onAction('trash') },
    { id: 'freshness', label: 'Stale docs', hint: 'never verified first, then the oldest verification', run: () => onAction('freshness') },
    { id: 'tree', label: 'Toggle file tree', keys: '⌘T', run: () => onAction('tree') },
    {
      id: 'import',
      label: 'Import a folder of Markdown…',
      hint: 'files become docs, folders become sections',
      run: () => {
        onAction('close')
        document.getElementById('import-folder-input')?.click()
      },
    },
    {
      id: 'memory-sync',
      label: 'Sync Claude Code memory now',
      hint: '~/.claude/projects/*/memory → Claude Memory (also runs every 10 min)',
      run: () => {
        onAction('close')
        api<{ files: number; imported: number; updated: number; unchanged: number; projects: number }>('/api/memory/sync', {
          method: 'POST',
        })
          .then((r) =>
            notify(
              `memory: ${r.files} files across ${r.projects} projects — ${r.imported} imported, ${r.updated} updated (in review), ${r.unchanged} unchanged`,
              'ok',
              { ttlMs: 10_000 },
            ),
          )
          .catch((e) => notify(errText(e)))
      },
    },
  ]
  if (docId) {
    cmds.push(
      {
        id: 'export-doc',
        label: 'Export this doc as Markdown…',
        hint: 'one .md file in ~/Downloads — for Slack, email, anywhere',
        run: () => {
          onAction('close')
          api<{ path: string }>(`/api/doc/${docId}/export`, { method: 'POST' })
            .then((r) => notify(`saved ${r.path}`, 'ok', { ttlMs: 12_000 }))
            .catch((e) => notify(errText(e)))
        },
      },
      {
        id: 'copy-doc',
        label: 'Copy this doc as Markdown',
        hint: 'to the clipboard',
        run: () => {
          onAction('close')
          api<{ markdown: string }>(`/api/doc/${docId}/markdown`)
            .then((r) => copyText(r.markdown))
            .then(() => notify('copied as Markdown', 'ok'))
            .catch((e) => notify(errText(e)))
        },
      },
    )
  }
  cmds.push(
    {
      id: 'export-vault',
      label: 'Export all docs as Markdown…',
      hint: 'a folder in ~/Downloads',
      run: () => {
        onAction('close')
        api<{ path: string; files: number }>('/api/export_vault', { method: 'POST' })
          .then((r) => notify(`exported ${r.files} files to ${r.path}`, 'ok', { ttlMs: 12_000 }))
          .catch((e) => notify(errText(e)))
      },
    },
    {
      id: 'backup',
      label: 'Back up database now',
      hint: 'daily snapshot, kept beside your notes',
      run: () => {
        onAction('close')
        api<{ path: string; bytes: number }>('/api/backups', { method: 'POST' })
          .then((r) => notify(`backup written: ${r.path} (${(r.bytes / 1_048_576).toFixed(1)} MB)`, 'ok', { ttlMs: 12_000 }))
          .catch((e) => notify(errText(e)))
      },
    },
  )
  // a native Save sheet needs the app; in a browser tab the row is absent
  if (inTauri()) {
    cmds.push({
      id: 'backup-to',
      label: 'Back up database to…',
      hint: 'one self-contained file wherever you choose — a USB stick, iCloud Drive, a sync folder',
      run: () => {
        onAction('close')
        saveDialog({
          title: 'Back up Grimoire database',
          defaultPath: backupFileName(),
          filters: [{ name: 'SQLite database', extensions: ['db'] }],
        })
          .then((to) => {
            if (!to) return
            return api<{ path: string; bytes: number }>('/api/backups', {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ to }),
            }).then((r) => notify(`backup written: ${r.path} (${(r.bytes / 1_048_576).toFixed(1)} MB)`, 'ok', { ttlMs: 12_000 }))
          })
          .catch((e) => notify(errText(e)))
      },
    })
  }
  cmds.push({
    id: 'reveal-backups',
    label: 'Show backups in Finder',
    hint: '~/.grimoire/backups — the folder to point Time Machine or a sync tool at',
    run: () => {
      onAction('close')
      api<{ dir: string }>('/api/backups/reveal', { method: 'POST' }).catch((e) => notify(errText(e)))
    },
  })
  return cmds
}
