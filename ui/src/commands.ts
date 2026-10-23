// The ⌘K action list, as data: what ActionsPalette ranks and runs. Actions
// only — places (Home, Review, Trash, Settings…) live in ⌘O, docs in ⌘O,
// content in ⌘⇧F. Rows that need a doc, a sign-in, the library or the desktop
// app are included only when those apply; `scope: 'doc'` rows list first.

import { notify, errText } from './Notice'
import { copyText } from './Profile'
import { backupFileName, inTauri, saveDialog } from './tauri'
import { api } from './types'
import type { OmniCommand } from './omni'
import { THEME_LABEL, getThemePref, nextThemePref, setThemePref } from './lapis/theme'

export type CommandAction =
  | 'tree'
  | 'newdoc'
  | 'capture'
  | 'close'
  | 'share'
  | 'books'
  | 'open'
  | 'search'
  | 'ask'
  | 'help'

export interface CommandCtx {
  /** SERVER mode, signed in through this browser: offer Sign out */
  signedIn?: boolean
  /** SERVER mode: the EPUB library exists (Add EPUBs) */
  library?: boolean
  onSignOut?: () => void
  /** the open doc, if any — enables the per-doc actions */
  docId: string | null
  onAction: (a: CommandAction) => void
}

export function buildCommands({ signedIn, library, onSignOut, docId, onAction }: CommandCtx): OmniCommand[] {
  const cmds: OmniCommand[] = []
  if (docId && signedIn) {
    cmds.push({ id: 'share-link', icon: 'link', scope: 'doc', label: 'Share link…', hint: 'a public, read-only snapshot of this doc', run: () => onAction('share') })
  }
  if (docId) {
    cmds.push(
      {
        id: 'export-doc', icon: 'download',
        scope: 'doc',
        label: 'Export this doc as Markdown…',
        hint: 'one .md file in ~/Downloads — for Slack, email, anywhere',
        keywords: 'download save file',
        run: () => {
          onAction('close')
          api<{ path: string }>(`/api/doc/${docId}/export`, { method: 'POST' })
            .then((r) => notify(`saved ${r.path}`, 'ok', { ttlMs: 12_000 }))
            .catch((e) => notify(errText(e)))
        },
      },
      {
        id: 'copy-doc', icon: 'copy',
        scope: 'doc',
        label: 'Copy this doc as Markdown',
        hint: 'to the clipboard',
        keywords: 'clipboard',
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
    { id: 'newdoc', icon: 'plus', group: 'Create', label: 'New doc…', keys: '⌘N', keywords: 'create page note', run: () => onAction('newdoc') },
    { id: 'capture', icon: 'capture', group: 'Create', label: 'Quick capture…', hint: 'a note straight into Inbox', keys: '⌘⇧I', keywords: 'inbox jot', run: () => onAction('capture') },
    { id: 'open', icon: 'search', group: 'Go', label: 'Open a doc or place…', keys: '⌘O', keywords: 'go to jump find title', run: () => onAction('open') },
    { id: 'search', icon: 'search', group: 'Go', label: 'Search inside docs…', hint: 'full text, typos fine', keys: '⌘⇧F', keywords: 'find content text', run: () => onAction('search') },
    { id: 'ask', icon: 'ask', group: 'Go', label: 'Ask the library…', hint: 'an answer doc that cites its blocks', keys: '⌘/', keywords: 'question vault answer ai', run: () => onAction('ask') },
  )
  if (library) {
    // the EPUB library is SERVER-mode only (LOCAL has no books routes)
    cmds.push({
      id: 'add-books', icon: 'upload', group: 'Create',
      label: 'Add EPUBs…',
      hint: 'upload files into the library',
      keywords: 'books upload import epub',
      run: () => {
        // the Books view renders the upload queue's progress and result
        // cards; the click stays inside the keypress's user gesture
        onAction('books')
        document.getElementById('book-upload-input')?.click()
      },
    })
  }
  cmds.push(
    { id: 'tree', icon: 'sidebar', group: 'Go', label: 'Show or hide the sidebar', keys: '⌘T', keywords: 'sidebar folders', run: () => onAction('tree') },
    {
      id: 'import', icon: 'folder', group: 'Create',
      label: 'Import a folder of Markdown…',
      hint: 'files become docs, folders become sections',
      run: () => {
        onAction('close')
        document.getElementById('import-folder-input')?.click()
      },
    },
    {
      id: 'memory-sync', icon: 'history', group: 'Data',
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
    {
      id: 'export-vault', icon: 'download', group: 'Data',
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
      id: 'backup', icon: 'download', group: 'Data',
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
      id: 'backup-to', icon: 'download', group: 'Data',
      label: 'Back up database to…',
      hint: 'one self-contained file wherever you choose — a USB stick, iCloud Drive, a sync folder',
      run: () => {
        onAction('close')
        saveDialog({
          title: 'Back up Taisce database',
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
    id: 'reveal-backups', icon: 'folder', group: 'Data',
    label: 'Show backups in Finder',
    hint: '~/.grimoire/backups — the folder to point Time Machine or a sync tool at',
    run: () => {
      onAction('close')
      api<{ dir: string }>('/api/backups/reveal', { method: 'POST' }).catch((e) => notify(errText(e)))
    },
  })
  cmds.push({
    id: 'theme',
    icon: 'sun',
    group: 'App',
    label: `Theme: ${THEME_LABEL[getThemePref()]} → ${THEME_LABEL[nextThemePref(getThemePref())]}`,
    hint: 'system, light or dark',
    keywords: 'appearance dark mode light mode colour',
    run: () => setThemePref(nextThemePref(getThemePref())),
  })
  cmds.push({ id: 'help', icon: 'keyboard', group: 'App', label: 'Keyboard shortcuts', keys: '?', keywords: 'keys cheatsheet help', run: () => onAction('help') })
  if (signedIn && onSignOut) {
    cmds.push({ id: 'sign-out', icon: 'signOut', group: 'App', label: 'Sign out', hint: 'end this browser’s session', keywords: 'log out logout', run: onSignOut })
  }
  return cmds
}
