// Settings › Share links (canvas Settings, #links; SERVER mode only): a
// table of the person's live links (doc, expiry, views, comments, Copy /
// Revoke…). A link with comments opens a row under it with the visitor
// comments: reply (as the owner) or delete; opening it marks them read.

import { Fragment, useCallback, useEffect, useState } from 'react'
import { errText, notify } from '../Notice'
import { copyText } from '../Profile'
import { relTime } from '../time'
import Link from '../Link'
import { isLive, revokeShare, type Share } from '../share'
import { deleteComment, expiresLabel, listAllShares, markCommentsRead, replyToComment, shareComments, threadComments, type ShareComment } from '../shareLinks'
import { Avatar, Button, ConfirmButton, Spinner } from '../lapis'
import { Row, SettingsCard } from './settingsParts'

function shortDate(iso: string): string {
  const t = new Date(iso)
  return Number.isNaN(t.getTime()) ? iso : t.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

export default function SettingsShareLinks({ dataVersion }: { dataVersion: number }) {
  const [list, setList] = useState<Share[] | null>(null)
  const [open, setOpen] = useState<string | null>(null)

  const load = useCallback(() => {
    listAllShares()
      .then((l) => setList(l.filter((s) => isLive(s))))
      .catch((e) => {
        setList([])
        notify(errText(e))
      })
  }, [])
  useEffect(load, [load, dataVersion])

  const revoke = async (s: Share) => {
    try {
      await revokeShare(s.id)
      notify(`revoked the link to “${s.title}”`, 'ok')
      if (open === s.id) setOpen(null)
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  return (
    <SettingsCard id="links" title="Share links" sub="Read-only links to a snapshot of one doc. Visitors can comment without an account.">
      {list === null && <Spinner />}
      {list !== null && list.length === 0 && (
        <Row first>
          <span className="muted">No live links. Share a doc from its page (Share…) to make one.</span>
        </Row>
      )}
      {list !== null && list.length > 0 && (
        <div className="st-table-wrap">
          <table className="st-tbl">
            <thead>
              <tr>
                <th>Doc</th>
                <th>Expires</th>
                <th>Views</th>
                <th>Comments</th>
                <th>
                  <span className="sr-only">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {list.map((s) => (
                <Fragment key={s.id}>
                  <tr>
                    <td>
                      <Link to={{ kind: 'doc', id: s.doc_id }} className="st-tbl-doc">
                        {s.title || 'Untitled'}
                      </Link>
                      <span className="st-tbl-sub">
                        made {shortDate(s.created_at)} · snapshot of v{s.revision}
                      </span>
                    </td>
                    <td>{expiresLabel(s.expires_at)}</td>
                    <td>{s.views}</td>
                    <td>
                      {s.comment_count > 0 ? (
                        <button type="button" className="st-cmt-toggle" aria-expanded={open === s.id} onClick={() => setOpen(open === s.id ? null : s.id)}>
                          {s.unread_comments > 0 ? (
                            <span className="st-new">{s.unread_comments} new</span>
                          ) : (
                            <span className="st-count">{s.comment_count}</span>
                          )}
                          <span className="sr-only">{open === s.id ? 'Hide comments' : 'Show comments'}</span>
                        </button>
                      ) : (
                        <span className="muted">{s.comments_enabled ? 'none' : 'off'}</span>
                      )}
                    </td>
                    <td className="st-tbl-actions">
                      <Button
                        variant="ghost"
                        size="sm"
                        aria-label={`Copy the link to ${s.title}`}
                        onClick={() =>
                          copyText(s.url)
                            .then(() => notify('link copied', 'ok'))
                            .catch((e) => notify(errText(e)))
                        }
                      >
                        Copy
                      </Button>
                      <ConfirmButton variant="ghost" confirmLabel="Revoke" onConfirm={() => revoke(s)}>
                        Revoke…
                      </ConfirmButton>
                    </td>
                  </tr>
                  {open === s.id && (
                    <tr>
                      <td colSpan={5} className="st-cmt-cell">
                        <Comments share={s} onChanged={load} />
                      </td>
                    </tr>
                  )}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </SettingsCard>
  )
}

function Comments({ share, onChanged }: { share: Share; onChanged: () => void }) {
  const [cs, setCs] = useState<ShareComment[] | null>(null)
  const load = useCallback(
    () =>
      shareComments(share.id)
        .then(setCs)
        .catch((e) => {
          setCs([])
          notify(errText(e))
        }),
    [share.id],
  )
  useEffect(() => {
    load().then(() => {
      // opening the row is reading them
      if (share.unread_comments > 0)
        markCommentsRead(share.id)
          .then(onChanged)
          .catch(() => {})
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [share.id])

  if (cs === null) return <Spinner />
  if (cs.length === 0) return <span className="muted">No comments.</span>
  return (
    <div className="st-cmts">
      {threadComments(cs).map(({ comment, replies }) => (
        <div key={comment.id} className="st-cmt">
          <CommentLine c={comment} share={share} onChanged={load} />
          {replies.map((r) => (
            <div key={r.id} className="st-cmt-reply">
              <CommentLine c={r} share={share} onChanged={load} />
            </div>
          ))}
          <ReplyBox share={share} parent={comment} onSent={load} />
        </div>
      ))}
    </div>
  )
}

function CommentLine({ c, share, onChanged }: { c: ShareComment; share: Share; onChanged: () => void }) {
  const who = c.is_owner ? 'You' : c.author || 'Guest'
  return (
    <div className="st-cmt-line">
      {c.is_owner ? <Avatar name={who} agent={false} size={26} /> : <span className="st-guest">{who.slice(0, 1).toUpperCase()}</span>}
      <div className="st-cmt-body">
        <b>{who}</b> <span className="muted st-cmt-when">{c.is_owner ? 'owner' : 'guest'} · {relTime(c.created_at)}</span>
        <div className="st-cmt-text">{c.body}</div>
      </div>
      <ConfirmButton
        variant="ghost"
        confirmLabel="Delete"
        onConfirm={() =>
          deleteComment(share.id, c.id)
            .then(onChanged)
            .catch((e) => notify(errText(e)))
        }
      >
        Delete
      </ConfirmButton>
    </div>
  )
}

function ReplyBox({ share, parent, onSent }: { share: Share; parent: ShareComment; onSent: () => void }) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const id = `st-reply-${parent.id}`
  const send = async () => {
    const body = text.trim()
    if (!body || busy) return
    setBusy(true)
    try {
      await replyToComment(share.id, body, parent.id)
      setText('')
      onSent()
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }
  return (
    <form
      className="st-reply"
      onSubmit={(e) => {
        e.preventDefault()
        send()
      }}
    >
      <label htmlFor={id} className="sr-only">
        Reply to {parent.author || 'the guest'}
      </label>
      <input id={id} className="st-inp st-inp-sm" placeholder="Reply…" value={text} onChange={(e) => setText(e.target.value)} />
      <Button type="submit" size="sm" disabled={!text.trim() || busy}>
        Reply
      </Button>
    </form>
  )
}
