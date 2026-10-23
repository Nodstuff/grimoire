// /settings[/<section>] (canvas Settings): the serif title, a section nav on
// the left (anchors that keep the URL in step) and one column of section
// cards: Profile, Appearance, E-readers, Upload tokens, Workspaces &
// members, Share links, Gardeners, Data, About. E-readers, Upload tokens
// and Share links exist in SERVER mode only (LOCAL has no library and no
// links). /settings/<section> scrolls to its card; ?tab=profile and
// ?tab=ereaders land on theirs (deeplink.ts tabTarget).

import { useCallback, useEffect, useRef, useState } from 'react'
import Profile, { copyDiagnostics, useVersion } from './Profile'
import Ereaders from './Ereaders'
import UploadTokens from './UploadTokens'
import ImportFolder from './ImportFolder'
import Link from './Link'
import { api, type GardenerRun, type Profile as ProfileRow } from './types'
import { errText, notify } from './Notice'
import { relTime } from './time'
import { inTauri } from './tauri'
import { Button, Kbd, buttonClass, useTheme, type ThemePref } from './lapis'
import SettingsWorkspaces from './screens/SettingsWorkspaces'
import SettingsShareLinks from './screens/SettingsShareLinks'
import { Row, RowText, Segmented, SettingsCard } from './screens/settingsParts'
import type { Workspace } from './workspaces'
import type { SettingsSection } from './deeplink'
import type { Gardener } from './Gardeners'
import './screens/settings.css'

export const SETTINGS_SECTIONS: { id: SettingsSection; label: string; serverOnly?: boolean }[] = [
  { id: 'profile', label: 'Profile' },
  { id: 'appearance', label: 'Appearance' },
  { id: 'ereaders', label: 'E-readers', serverOnly: true },
  { id: 'uploads', label: 'Upload tokens', serverOnly: true },
  { id: 'workspaces', label: 'Workspaces & members' },
  { id: 'links', label: 'Share links', serverOnly: true },
  { id: 'gardeners', label: 'Gardeners' },
  { id: 'data', label: 'Data' },
  { id: 'about', label: 'About' },
]

/** The sections this mode shows, and the one a request lands on (a
 * server-only section asked for in LOCAL mode falls back to the first). */
export function settingsSections(server: boolean, want: SettingsSection) {
  const shown = SETTINGS_SECTIONS.filter((s) => server || !s.serverOnly)
  const active = shown.some((s) => s.id === want) ? want : shown[0].id
  return { shown, active }
}

export default function Settings({
  section,
  server,
  onSection,
  dataVersion,
  version,
  onProfileChanged,
  onSignOut,
  workspaces = [],
  onWorkspacesChanged,
  onShortcuts,
  onImported,
}: {
  section: SettingsSection
  /** SERVER mode: E-readers, Upload tokens and Share links show */
  server: boolean
  /** the nav moved to a section (App keeps the URL in step, replacing) */
  onSection: (s: SettingsSection) => void
  dataVersion: number
  version: string | null
  onProfileChanged: (p: ProfileRow) => void
  onSignOut?: () => void
  workspaces?: Workspace[]
  onWorkspacesChanged?: () => void
  /** opens the keyboard shortcuts sheet */
  onShortcuts?: () => void
  /** a Markdown folder import landed */
  onImported?: () => void
}) {
  const { shown, active } = settingsSections(server, section)
  const [here, setHere] = useState<SettingsSection>(active)

  // /settings/<section>: scroll its card into view (the first one is the top)
  const first = useRef(true)
  useEffect(() => {
    setHere(active)
    const el = document.getElementById(`settings-${active}`)
    if (!el) return
    if (first.current && active === shown[0].id) {
      first.current = false
      return
    }
    el.scrollIntoView({ behavior: first.current ? 'auto' : 'smooth', block: 'start' })
    first.current = false
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active])

  // the nav follows the reading position (the card nearest the top)
  useEffect(() => {
    if (typeof IntersectionObserver === 'undefined') return
    const io = new IntersectionObserver(
      (entries) => {
        const top = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0]
        const id = top?.target.getAttribute('data-section') as SettingsSection | null
        if (id) setHere(id)
      },
      { rootMargin: '0px 0px -65% 0px' },
    )
    document.querySelectorAll('.st-sections > [data-section]').forEach((el) => io.observe(el))
    return () => io.disconnect()
  }, [shown.length])

  return (
    <div className="st-page">
      <h1 className="st-title">Settings</h1>
      <div className="st-layout">
        <nav className="st-nav" aria-label="Settings sections">
          {shown.map((s) => (
            <a
              key={s.id}
              href={s.id === 'profile' ? '/settings' : `/settings/${s.id}`}
              className={s.id === here ? 'on' : ''}
              aria-current={s.id === here ? 'true' : undefined}
              onClick={(e) => {
                if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
                e.preventDefault()
                setHere(s.id)
                if (s.id === active) document.getElementById(`settings-${s.id}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
                else onSection(s.id)
              }}
            >
              {s.label}
            </a>
          ))}
        </nav>

        <div className="st-sections">
          <Profile dataVersion={dataVersion} onChanged={onProfileChanged} onSignOut={onSignOut} />
          <Appearance />
          {server && <Ereaders dataVersion={dataVersion} />}
          {server && <UploadTokens dataVersion={dataVersion} />}
          <SettingsWorkspaces workspaces={workspaces} server={server} onChanged={() => onWorkspacesChanged?.()} />
          {server && <SettingsShareLinks dataVersion={dataVersion} />}
          <GardenersSummary server={server} dataVersion={dataVersion} />
          <DataSection server={server} onImported={onImported} />
          <About version={version} onShortcuts={onShortcuts} />
        </div>
      </div>
    </div>
  )
}

const hostName = () => (typeof location === 'undefined' ? 'the server' : location.host)

const THEME_OPTIONS: { id: ThemePref; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
]

function Appearance() {
  const { pref, setPref } = useTheme()
  return (
    <SettingsCard id="appearance" title="Appearance">
      <Row first>
        <RowText sub="System follows your device’s light or dark setting.">Theme</RowText>
        <Segmented label="Theme" value={pref} options={THEME_OPTIONS} onChange={setPref} />
      </Row>
    </SettingsCard>
  )
}

/** What a gardener kind does, in a line. */
const KIND_LINE: Record<string, string> = {
  tagging: 'Adds tags to new docs',
  auditor: 'Audits docs against their sources',
  filer: 'Suggests where Inbox notes belong',
  reviewer: 'Retired reviewer',
}

/** One row per gardener: its name, what it does and when, and its last run.
 * From the registry when this app can read it (LOCAL), else from the runs. */
export function gardenerRows(gardeners: Gardener[] | null, runs: GardenerRun[]): { name: string; what: string; last: GardenerRun | null; enabled: boolean }[] {
  const latest = new Map<string, GardenerRun>()
  for (const r of runs) {
    const cur = latest.get(r.gardener_name)
    if (!cur || r.started_at > cur.started_at) latest.set(r.gardener_name, r)
  }
  if (gardeners && gardeners.length > 0)
    return gardeners.map((g) => ({
      name: g.name,
      what: [KIND_LINE[g.kind] ?? g.kind, g.schedule].filter(Boolean).join(' · '),
      last: latest.get(g.name) ?? null,
      enabled: g.enabled,
    }))
  return [...latest.values()]
    .sort((a, b) => a.gardener_name.localeCompare(b.gardener_name))
    .map((r) => ({ name: r.gardener_name, what: r.summary?.split('\n')[0] ?? '', last: r, enabled: true }))
}

function GardenersSummary({ server, dataVersion }: { server: boolean; dataVersion: number }) {
  const [gardeners, setGardeners] = useState<Gardener[] | null>(null)
  const [runs, setRuns] = useState<GardenerRun[] | null>(null)
  useEffect(() => {
    // the registry is behind the admin token: answered in the Mac app, not on the server
    if (!server)
      api<Gardener[]>('/admin/gardeners')
        .then(setGardeners)
        .catch(() => setGardeners(null))
    api<GardenerRun[]>('/api/runs')
      .then(setRuns)
      .catch(() => setRuns([]))
  }, [server, dataVersion])
  const rows = gardenerRows(gardeners, runs ?? [])
  const sub = server ? (
    <>
      Scheduled agents. On {hostName()} they are managed on the server (<span className="mono">taisce gardener …</span>); here you see what they do.
    </>
  ) : (
    <>
      Scheduled agents on this Mac. Change, run or add them on the <Link to={{ kind: 'runs' }}>Gardeners</Link> page.
    </>
  )
  return (
    <SettingsCard id="gardeners" title="Gardeners" sub={sub}>
      <div>
        {runs !== null && rows.length === 0 && (
          <Row first>
            <span className="muted">No gardeners have run yet.</span>
          </Row>
        )}
        {rows.map((g, i) => (
          <Row key={g.name} first={i === 0}>
            <span className="mono st-gd-name">{g.name}</span>
            <span className="st-row-text">{g.what || <span className="muted">—</span>}</span>
            <span className={`st-gd-last${g.last?.status === 'running' ? ' running' : ''}`}>
              {!g.enabled ? 'off' : g.last ? `${g.last.status === 'ok' ? 'ok' : g.last.status.replace('-', ' ')} · ${relTime(g.last.started_at)}` : 'not run yet'}
            </span>
          </Row>
        ))}
        <Row>
          <Link to={{ kind: 'runs' }} className={buttonClass('secondary', 'sm')}>
            Open Gardeners
          </Link>
        </Row>
      </div>
    </SettingsCard>
  )
}

interface BackupList {
  dir: string
  backups: { path: string; date: string; bytes: number }[]
}

function DataSection({ server, onImported }: { server: boolean; onImported?: () => void }) {
  // the backup list is the owner's (admin) surface: its answer says whether
  // Export and Back up are this person's to press
  const [backups, setBackups] = useState<BackupList | null | undefined>(undefined)
  const load = useCallback(() => {
    api<BackupList>('/api/backups')
      .then(setBackups)
      .catch(() => setBackups(null))
  }, [])
  useEffect(load, [load])
  // unknown until the list answers: the owner's buttons wait for it
  const admin = !!backups
  const last = backups?.backups[0]

  const exportAll = () =>
    api<{ path: string; files: number }>('/api/export_vault', { method: 'POST' })
      .then((r) => notify(`exported ${r.files} files to ${r.path}`, 'ok', { ttlMs: 12_000 }))
      .catch((e) => notify(errText(e)))
  const backUp = () =>
    api<{ path: string; bytes: number }>('/api/backups', { method: 'POST' })
      .then((r) => {
        notify(`backup written: ${r.path} (${(r.bytes / 1_048_576).toFixed(1)} MB)`, 'ok', { ttlMs: 12_000 })
        load()
      })
      .catch((e) => notify(errText(e)))

  return (
    <SettingsCard id="data" title="Data">
      <div>
        {admin && (
          <Row first>
            <RowText sub={server ? 'A folder of .md files in the server’s ~/Downloads' : 'A folder of .md files in ~/Downloads'}>Download every doc as Markdown</RowText>
            <Button size="sm" onClick={exportAll}>
              Export all
            </Button>
          </Row>
        )}
        <Row first={!admin}>
          <RowText sub="Each file becomes a doc; folders become folders">Import a folder of Markdown</RowText>
          <ImportFolder label="Import…" className={buttonClass('secondary', 'sm')} onDone={() => onImported?.()} />
        </Row>
        <Row>
          <RowText
            sub={
              admin
                ? last
                  ? `Daily snapshots beside the database · last ${last.date} (${(last.bytes / 1_048_576).toFixed(1)} MB)`
                  : 'Daily snapshots beside the database · none yet'
                : 'Kept by the server: ask its owner for a copy'
            }
          >
            Backups
          </RowText>
          {admin && (
            <Button size="sm" onClick={backUp}>
              Back up now
            </Button>
          )}
          {admin && inTauri() && (
            <Button
              size="sm"
              variant="ghost"
              onClick={() => api('/api/backups/reveal', { method: 'POST' }).catch((e) => notify(errText(e)))}
            >
              Show in Finder
            </Button>
          )}
        </Row>
      </div>
    </SettingsCard>
  )
}

function About({ version: appVersion, onShortcuts }: { version: string | null; onShortcuts?: () => void }) {
  const version = useVersion(appVersion)
  return (
    <SettingsCard id="about" title="About">
      <Row first>
        <span className="st-row-text">
          Taisce <span className="mono muted st-mono">{version ?? '—'}</span>
          <span className="st-row-sub">If something goes wrong, copy the diagnostics and send them along.</span>
        </span>
        <Button size="sm" onClick={copyDiagnostics}>
          Copy diagnostics
        </Button>
        {onShortcuts && (
          <Button variant="ghost" size="sm" onClick={onShortcuts}>
            Keyboard shortcuts <Kbd>?</Kbd>
          </Button>
        )}
      </Row>
    </SettingsCard>
  )
}
