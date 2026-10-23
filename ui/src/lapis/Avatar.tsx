import { AgentGlyph } from './icons'

/** Is this principal an agent? `claude:docs`, `keeper:API`, a gardener name
 * with a kind of 'agent'… People are plain names. */
export function isAgentName(name: string, kind?: string | null): boolean {
  if (kind) return kind !== 'human' && kind !== 'person' && kind !== 'user'
  return name.includes(':')
}

export function initialOf(name: string): string {
  const t = name.trim()
  return t ? t[0].toUpperCase() : '?'
}

/** Who wrote it. A person is a round lapis avatar with an initial; an agent
 * is a violet rounded square with the star glyph, and (with `showName`) its
 * name in mono violet. */
export function Avatar({
  name,
  agent,
  kind,
  size = 22,
  showName = false,
}: {
  name: string
  /** force agent/person; otherwise inferred from `kind` or the name */
  agent?: boolean
  kind?: string | null
  size?: number
  showName?: boolean
}) {
  const isAgent = agent ?? isAgentName(name, kind)
  const mark = isAgent ? (
    <span className="lp-avatar lp-avatar-agent" style={{ width: size, height: size }} aria-hidden={showName || undefined} title={showName ? undefined : name}>
      <AgentGlyph size={Math.round(size * 0.58)} />
    </span>
  ) : (
    <span
      className="lp-avatar lp-avatar-person"
      style={{ width: size, height: size, fontSize: Math.round(size / 2) }}
      aria-hidden={showName || undefined}
      title={showName ? undefined : name}
    >
      {initialOf(name)}
    </span>
  )
  if (!showName) return mark
  return (
    <span className="lp-who">
      {mark}
      <span className={isAgent ? 'lp-agent-name' : 'lp-person-name'}>{name}</span>
    </span>
  )
}

/** An agent's name alone, in mono violet. */
export function AgentName({ name }: { name: string }) {
  return <span className="lp-agent-name">{name}</span>
}
