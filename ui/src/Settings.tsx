// Settings (⌘, · the gear · ⌘O → Settings · ?tab=settings): one page with a
// section per concern. Profile is the old profile view; E-readers the old
// e-readers view and Upload tokens (book upload tokens for scripts), both
// SERVER mode only (LOCAL has no library). ?tab=profile and
// ?tab=ereaders land on their sections (deeplink.ts tabTarget).

import Profile from './Profile'
import Ereaders from './Ereaders'
import UploadTokens from './UploadTokens'
import type { Profile as ProfileRow } from './types'
import type { SettingsSection } from './deeplink'

export const SETTINGS_SECTIONS: { id: SettingsSection; label: string; serverOnly?: boolean }[] = [
  { id: 'profile', label: 'Profile' },
  { id: 'ereaders', label: 'E-readers', serverOnly: true },
  { id: 'uploads', label: 'Upload tokens', serverOnly: true },
]

/** The sections this mode shows, and the one to render for a request (a
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
}: {
  section: SettingsSection
  /** SERVER mode: E-readers and Upload tokens show */
  server: boolean
  onSection: (s: SettingsSection) => void
  dataVersion: number
  version: string | null
  onProfileChanged: (p: ProfileRow) => void
  onSignOut?: () => void
}) {
  const { shown, active } = settingsSections(server, section)
  return (
    <div className="queue settings">
      <h1 className="queue-title">settings</h1>
      {shown.length > 1 && (
        <div className="settings-tabs" role="tablist">
          {shown.map((s) => (
            <button
              key={s.id}
              role="tab"
              aria-selected={s.id === active}
              className={`chip ${s.id === active ? 'on' : ''}`}
              onClick={() => onSection(s.id)}
            >
              {s.label}
            </button>
          ))}
        </div>
      )}
      {active === 'profile' && (
        <Profile embedded dataVersion={dataVersion} onChanged={onProfileChanged} version={version} onSignOut={onSignOut} />
      )}
      {active === 'ereaders' && <Ereaders embedded dataVersion={dataVersion} />}
      {active === 'uploads' && <UploadTokens dataVersion={dataVersion} />}
    </div>
  )
}
