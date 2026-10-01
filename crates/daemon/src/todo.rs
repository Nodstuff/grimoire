//! The per-day To-do list behind the briefing home (replaces the old "Today"
//! section, which parsed the daily log's Plans — those logs are agent thought
//! dumps, not a checklist).
//!
//! Storage: one ROOT doc titled **To-do** (find-or-create by title, never
//! duplicated). One `## YYYY-MM-DD` heading per day, kept in date order, and
//! ONE BLOCK PER ITEM under it:
//!
//! ```markdown
//! ## 2026-09-10
//!
//! - [ ] Ship 0.8.0 · due 2026-09-12
//!   needs Tom's sign-off after the local trial
//!
//! - [x] call the bank (carried from 2026-09-09)
//! ```
//!
//! - `[ ]` open, `[x]` done, `[>]` moved forward by carry-forward.
//! - ` · due YYYY-MM-DD` is the DEADLINE (the day heading is the scheduled
//!   day). Typed as any trailing `due <when>` / `by <when>` phrase — `due fri`,
//!   `by 12/9` (DAY/MONTH), `due 12 sep`, `due in 3 days`, `due next mon` — see
//!   `due.rs` for the grammar; it is resolved against the CLIENT's `today` and
//!   stored in ISO form. A time may follow (`due fri 3pm`, `by tomorrow
//!   09:30`): it is the client's wall time at its `utc_offset`, stored as a UTC
//!   instant ` · due 2026-10-03T14:00Z`. A date alone stays a floating all-day
//!   date. The server never uses a local timezone. The pre-UTC
//!   ` · due YYYY-MM-DD HH:MM` is read as UTC and flagged `legacy_time`, never
//!   rewritten. The pre-0.8 `⏰ YYYY-MM-DD` token is still read and rewritten
//!   to the new form the next time its day is written.
//! - `(carried from YYYY-MM-DD)` at the end names the day the item was first
//!   scheduled; carry-forward keeps the original stamp.
//! - Indented lines under the item, inside the same block, are its NOTE.
//!
//! That is plain markdown an agent writes with `append(todo_doc, "- [ ] x",
//! to: "2026-09-10", create_missing: true)`, so the markdown is the contract:
//! every write re-reads the doc's markdown, edits the text (re-rendering only
//! the touched day's section canonically — one blank-line-separated block per
//! item), diffs it back into ops and proposes them at the current epoch AS THE
//! HUMAN — the path DocEditor / `propose_markdown` take (greens, applied
//! directly, one epoch bump). A doc in a live session is left alone.
//!
//! An agent's `append(todo, "- [ ] x due fri")` is read with the phrase
//! resolved (deadline shown, text without it) but the line is left as typed
//! until the next write to THAT day — a GET never rewrites, and untouched
//! days keep their blocks byte for byte.
//!
//! Item ids: `<index among the day's items>-<fnv1a of the text>`, e.g.
//! `3-9a1f0c2e`. The index makes duplicates addressable; the hash catches the
//! list having shifted under the client (an agent appended above) — an id
//! whose index no longer matches falls back to the single item with that
//! hash, and errors when there is none.
//!
//! Carry-forward: a `GET` for today (a date ≥ the client's `today`, with no
//! newer day in the doc) copies the still-open `[ ]` items of the most recent
//! earlier day into today's section — deadline and note intact, stamped
//! `(carried from <original day>)` — and marks the sources `[>]`. A repeat GET
//! finds no `[ ]` there and copies nothing, so it is idempotent even when an
//! agent already started today's section; a read of a past day never moves
//! anything.
//!
//! Client context: the server has no idea what day it is for you. Every
//! route takes the client's `today` (`YYYY-MM-DD`, query or body) — REQUIRED
//! in SERVER mode; in LOCAL mode it falls back to the UTC date — and, for
//! anything that reads a typed time, its `utc_offset` (`+01:00`); a typed time
//! with no offset is refused. Deadlines in answers: `deadline` (`YYYY-MM-DD`)
//! for all-day items, `due_at` (RFC 3339 UTC) for timed ones, `legacy_time:
//! true` on a pre-UTC one. No `overdue`: clients compute it in their zone.
//!
//! Routes (all human-principal):
//! - `GET  /api/todo?date=YYYY-MM-DD&today=YYYY-MM-DD` → `{doc_id, date,
//!   today, items:[{id, text, done, carried?, carried_from?, deadline?,
//!   due_at?, legacy_time?, note?, due_soon}], carried, prev_date, epoch}`
//! - `POST /api/todo {date, text, today, utc_offset}` add; a trailing
//!   `due <when>` / `by <when>` phrase becomes the deadline. One that looks
//!   like a date but does not parse leaves the text as typed and adds
//!   `warning: "couldn't read that date"` to the answer.
//! - `POST /api/todo/toggle {date, item_id | text, done, today}`
//! - `POST /api/todo/edit {date, item_id, text, today, utc_offset}` (same
//!   phrase handling; a text without a phrase keeps the item's deadline)
//! - `GET  /api/todo/parse?text=…&today=…&utc_offset=…` → `{text, deadline |
//!   null, due_at | null, warning?}` — the UI's live hint, so the rules live
//!   here only
//! - `POST /api/todo/remove {date, item_id, today}`
//! - `POST /api/todo/move {date, item_id, to_date, today}` (heading created in
//!   date order)
//! - `POST /api/todo/deadline {date, item_id, deadline | due_at | neither,
//!   today}` — `deadline: "YYYY-MM-DD"` makes it all-day, `due_at` (RFC 3339
//!   with a zone) timed, neither clears
//! - `GET  /api/todo/due?until=<YYYY-MM-DD | RFC 3339>` → `{doc_id | null,
//!   epoch, now (UTC), until, default_alert_time, items: [{date, id, text,
//!   deadline? | due_at?, legacy_time?, note?, carried_from?}]}` — every OPEN
//!   item with a deadline, across all days, soonest first (an all-day item
//!   sorts at the start of its date); a date `until` keeps all-day deadlines
//!   on or before it and timed ones whose UTC date is; an instant keeps timed
//!   ones at or before it and all-day ones on or before its UTC date. No
//!   local-time reading of either. READ-ONLY: no carry-forward, and no To-do
//!   doc is created. Errors are 400/500 with `{error}`.
//! - `POST /api/todo/note {date, item_id, note, today}` (empty clears)

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use crate::due::Due;
use chrono::{FixedOffset, NaiveDate};
use grimoire_store::{BlockStore, Doc, SqliteStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const TODO_TITLE: &str = "To-do";
/// the pre-0.8 deadline token, read but never written
const CLOCK: &str = "⏰";
const STAMP: &str = "(carried from ";
/// A deadline this close (in days) is `due_soon`.
const DUE_SOON_DAYS: i64 = 2;

/* ---------- the model ---------- */

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRec {
    /// ' ' open, 'x' done, '>' moved forward
    pub mark: char,
    pub text: String,
    /// as stored after ` · due ` (`due::Due::stored`), kept verbatim
    pub deadline: Option<String>,
    pub carried_from: Option<String>,
    /// note lines, de-indented, joined by '\n'
    pub note: Option<String>,
    /// the block exactly as read; rendered verbatim until its day is
    /// written, so a GET never rewrites and other days are left alone
    pub raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Item(ItemRec),
    /// a non-item block under the day heading, kept verbatim
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Chunk {
    /// anything outside a day section (frontmatter, a title, other headings)
    Other(Vec<String>),
    Day { date: String, entries: Vec<Entry> },
}

/// What the API hands out for one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: String,
    pub text: String,
    pub done: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub carried: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
    /// an all-day deadline, `YYYY-MM-DD`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// a timed deadline, RFC 3339 UTC
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    /// a pre-UTC `YYYY-MM-DD HH:MM` deadline, read as UTC
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy_time: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// open, due within `DUE_SOON_DAYS` of the client's `today` (a timed
    /// deadline by its UTC date)
    pub due_soon: bool,
}

/* ---------- pure: parsing ---------- */

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn item_id(index: usize, text: &str) -> String {
    format!("{index}-{:08x}", fnv1a(text.trim()))
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter()
            .enumerate()
            .all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// `## 2026-09-10` → Some("2026-09-10")
fn day_heading(line: &str) -> Option<&str> {
    let d = line.trim().strip_prefix("## ")?.trim();
    is_date(d).then_some(d)
}

fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

/// `- [x] rest` → Some(('x', "rest")); tolerant of `*` bullets and indent.
fn split_item_line(line: &str) -> Option<(char, &str)> {
    let t = line.trim_start();
    let rest = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?.trim_start();
    let mut c = rest.chars();
    if c.next()? != '[' {
        return None;
    }
    let mark = c.next()?;
    if !matches!(mark, ' ' | 'x' | 'X' | '>') || c.next()? != ']' {
        return None;
    }
    let mark = if mark == 'X' { 'x' } else { mark };
    Some((mark, c.as_str().trim()))
}

/// What `split_tokens` reads off an item's first line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Tokens {
    text: String,
    deadline: Option<String>,
    carried_from: Option<String>,
    /// a trailing date-like phrase that did not parse (text left as typed)
    warning: Option<&'static str>,
}

/// `today` is validated where it enters (`client`); UTC is only the
/// unreachable fallback, never a local clock.
fn today_date(today: &str) -> NaiveDate {
    parse_date(today).unwrap_or_else(|| chrono::Utc::now().date_naive())
}

/// Pull the trailing `(carried from D)` stamp, the legacy `⏰ D` token and a
/// trailing `due <when>` / `by <when>` phrase (resolved against `today`, a
/// typed time at `zone`) out of an item's first line.
fn split_tokens(raw: &str, today: NaiveDate, zone: Option<FixedOffset>) -> Tokens {
    let mut t = raw.trim().to_string();
    let mut from = None;
    if t.ends_with(')')
        && let Some(open) = t.rfind(STAMP)
    {
        let d = t[open + STAMP.len()..t.len() - 1].to_string();
        if is_date(&d) {
            from = Some(d);
            t.truncate(open);
        }
    }
    let mut deadline = None;
    if let Some(at) = t.find(CLOCK) {
        let after = t[at + CLOCK.len()..].trim_start();
        let cand: String = after.chars().take(10).collect();
        if is_date(&cand) {
            deadline = Some(cand.clone());
            let tail = after[cand.len()..].to_string();
            t = format!("{} {}", t[..at].trim_end(), tail.trim_start());
        }
    }
    let mut warning = None;
    let parsed = crate::due::split_due(&t, today, zone);
    let text = if deadline.is_none() {
        deadline = parsed.due.map(|d| d.stored());
        warning = parsed.warning;
        parsed.text
    } else {
        t.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    Tokens { text, deadline, carried_from: from, warning }
}

fn parse_item(block: &[&str], today: NaiveDate) -> Option<ItemRec> {
    let (mark, first) = split_item_line(block[0])?;
    // read out of the doc: no client zone (a typed time there is legacy)
    let Tokens { text, deadline, carried_from, .. } = split_tokens(first, today, None);
    let note_lines: Vec<&str> = block[1..]
        .iter()
        .map(|l| l.strip_prefix("  ").or_else(|| l.strip_prefix('\t')).unwrap_or(l.trim_start()))
        .collect();
    let note = (!note_lines.is_empty()).then(|| note_lines.join("\n").trim_end().to_string());
    Some(ItemRec {
        mark,
        text,
        deadline,
        carried_from,
        note: note.filter(|n| !n.is_empty()),
        raw: Some(block.iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")),
    })
}

/// A day's body (lines after the heading) → entries. A block is a run of
/// non-blank lines; a checkbox line always starts a new block, so an agent's
/// contiguous `- [ ] a\n- [ ] b` list still yields two items.
fn parse_entries(lines: &[&str], today: NaiveDate) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < lines.len() && !lines[i].trim().is_empty() && split_item_line(lines[i]).is_none() {
            i += 1;
        }
        let block = &lines[start..i];
        match parse_item(block, today) {
            Some(it) => out.push(Entry::Item(it)),
            None => out.push(Entry::Text(block.join("\n"))),
        }
    }
    out
}

fn parse_doc(md: &str, today: NaiveDate) -> Vec<Chunk> {
    let lines: Vec<&str> = md.lines().collect();
    let mut chunks = Vec::new();
    let mut i = 0;
    let mut other: Vec<String> = Vec::new();
    while i < lines.len() {
        if let Some(date) = day_heading(lines[i]) {
            if !other.is_empty() {
                chunks.push(Chunk::Other(std::mem::take(&mut other)));
            }
            let start = i + 1;
            let mut end = start;
            while end < lines.len() && !(is_heading(lines[end]) && lines[end].trim_start().starts_with("##") && !lines[end].trim_start().starts_with("###")) && !lines[end].trim_start().starts_with("# ") {
                end += 1;
            }
            chunks.push(Chunk::Day { date: date.to_string(), entries: parse_entries(&lines[start..end], today) });
            i = end;
        } else {
            other.push(lines[i].to_string());
            i += 1;
        }
    }
    if !other.is_empty() {
        chunks.push(Chunk::Other(other));
    }
    chunks
}

/* ---------- pure: rendering ---------- */

/// The canonical line: `- [ ] text · due D (carried from D)` + indented note.
/// An item whose day was not written renders exactly as it was read.
fn render_item(it: &ItemRec) -> String {
    if let Some(raw) = &it.raw {
        return raw.clone();
    }
    let mut line = format!("- [{}] {}", it.mark, it.text);
    if let Some(d) = &it.deadline {
        line.push_str(&format!(" {} due {d}", crate::due::SEP));
    }
    if let Some(f) = &it.carried_from {
        line.push_str(&format!(" {STAMP}{f})"));
    }
    if let Some(n) = &it.note {
        for l in n.lines() {
            line.push('\n');
            if !l.trim().is_empty() {
                line.push_str("  ");
                line.push_str(l);
            }
        }
    }
    line
}

fn render_doc(chunks: &[Chunk]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in chunks {
        match c {
            Chunk::Other(lines) => {
                let s = lines.join("\n");
                let s = s.trim_matches('\n');
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
            Chunk::Day { date, entries } => {
                parts.push(format!("## {date}"));
                for e in entries {
                    parts.push(match e {
                        Entry::Item(it) => render_item(it),
                        Entry::Text(t) => t.clone(),
                    });
                }
            }
        }
    }
    let mut md = parts.join("\n\n");
    if !md.is_empty() {
        md.push('\n');
    }
    md
}

/* ---------- pure: queries ---------- */

fn day_index(chunks: &[Chunk], date: &str) -> Option<usize> {
    chunks.iter().position(|c| matches!(c, Chunk::Day { date: d, .. } if d == date))
}

/// Index of `## date`, creating it in date order when missing.
fn ensure_day(chunks: &mut Vec<Chunk>, date: &str) -> usize {
    if let Some(i) = day_index(chunks, date) {
        return i;
    }
    let at = chunks
        .iter()
        .position(|c| matches!(c, Chunk::Day { date: d, .. } if d.as_str() > date))
        .or_else(|| {
            chunks
                .iter()
                .rposition(|c| matches!(c, Chunk::Day { .. }))
                .map(|i| i + 1)
        })
        .unwrap_or(chunks.len());
    chunks.insert(at, Chunk::Day { date: date.to_string(), entries: Vec::new() });
    at
}

/// Mark the day as written: every item re-renders canonically (legacy `⏰`
/// and typed phrases become ` · due YYYY-MM-DD`).
fn touch_day(chunks: &mut [Chunk], i: usize) {
    for e in entries_mut(chunks, i) {
        if let Entry::Item(it) = e {
            it.raw = None;
        }
    }
}

fn entries_of<'a>(chunks: &'a [Chunk], date: &str) -> Option<&'a Vec<Entry>> {
    day_index(chunks, date).and_then(|i| match &chunks[i] {
        Chunk::Day { entries, .. } => Some(entries),
        _ => None,
    })
}

fn entries_mut(chunks: &mut [Chunk], i: usize) -> &mut Vec<Entry> {
    match &mut chunks[i] {
        Chunk::Day { entries, .. } => entries,
        _ => unreachable!("day index points at a day"),
    }
}

fn items_of(entries: &[Entry]) -> Vec<&ItemRec> {
    entries
        .iter()
        .filter_map(|e| match e {
            Entry::Item(it) => Some(it),
            _ => None,
        })
        .collect()
}

/// Every day heading in the doc, in document order.
fn days(chunks: &[Chunk]) -> Vec<&str> {
    chunks
        .iter()
        .filter_map(|c| match c {
            Chunk::Day { date, .. } => Some(date.as_str()),
            _ => None,
        })
        .collect()
}

fn to_item(index: usize, it: &ItemRec, today: &str) -> Item {
    let open = it.mark == ' ';
    let due = it.deadline.as_deref().and_then(crate::due::parse_stored);
    let due_soon = match (due, parse_date(today)) {
        (Some(d), Some(t)) if open => (0..=DUE_SOON_DAYS).contains(&(d.utc_date() - t).num_days()),
        _ => false,
    };
    Item {
        id: item_id(index, &it.text),
        text: it.text.clone(),
        done: it.mark == 'x',
        carried: it.mark == '>',
        carried_from: it.carried_from.clone(),
        deadline: due.and_then(|d| d.day()).map(|d| d.to_string()),
        due_at: due.and_then(|d| d.instant()).map(crate::due::rfc3339),
        legacy_time: due.is_some_and(|d| d.is_legacy()),
        note: it.note.clone(),
        due_soon,
    }
}

/// The items under `## date` (empty when the section is absent).
pub fn parse_day(markdown: &str, date: &str, today: &str) -> Vec<Item> {
    let chunks = parse_doc(markdown, today_date(today));
    entries_of(&chunks, date)
        .map(|es| items_of(es).into_iter().enumerate().map(|(i, it)| to_item(i, it, today)).collect())
        .unwrap_or_default()
}

/// The newest day strictly before `date` that has a section.
pub fn prev_day(markdown: &str, date: &str) -> Option<String> {
    let chunks = parse_doc(markdown, today_date(date));
    days(&chunks).into_iter().filter(|d| *d < date).max().map(str::to_string)
}

/// Position in `entries` of the item `id` names: its index when the hash
/// still matches there, else the single item with that hash.
fn locate(entries: &[Entry], id: &str) -> Result<usize, String> {
    let (idx, want) = id.split_once('-').ok_or_else(|| format!("bad item id: {id}"))?;
    let positions: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Entry::Item(_)))
        .map(|(p, _)| p)
        .collect();
    let hash_at = |p: usize| match &entries[p] {
        Entry::Item(it) => format!("{:08x}", fnv1a(it.text.trim())) == want,
        _ => false,
    };
    if let Ok(i) = idx.parse::<usize>()
        && let Some(&p) = positions.get(i)
        && hash_at(p)
    {
        return Ok(p);
    }
    let hits: Vec<usize> = positions.into_iter().filter(|&p| hash_at(p)).collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err("item not found (the list changed — reload)".into()),
        _ => Err("ambiguous item (duplicates changed places — reload)".into()),
    }
}

fn find_entry(entries: &[Entry], id: Option<&str>, text: Option<&str>) -> Result<usize, String> {
    if let Some(id) = id {
        return locate(entries, id);
    }
    let text = text.ok_or("item_id or text required")?.trim();
    entries
        .iter()
        .position(|e| matches!(e, Entry::Item(it) if it.text == text))
        .ok_or_else(|| format!("no item “{text}”"))
}

/* ---------- pure: edits (markdown in, markdown out) ---------- */

fn with_item(
    md: &str,
    date: &str,
    today: &str,
    id: Option<&str>,
    text: Option<&str>,
    f: impl FnOnce(&mut ItemRec),
) -> Result<String, String> {
    let mut chunks = parse_doc(md, today_date(today));
    let i = day_index(&chunks, date).ok_or_else(|| format!("no to-do list for {date}"))?;
    touch_day(&mut chunks, i);
    let entries = entries_mut(&mut chunks, i);
    let p = find_entry(entries, id, text)?;
    if let Entry::Item(it) = &mut entries[p] {
        f(it);
    }
    Ok(render_doc(&chunks))
}

/// One open, deadlined item for `GET /api/todo/due`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DueItem {
    /// the day heading it sits under (address it with `{date, item_id}`)
    pub date: String,
    pub id: String,
    pub text: String,
    /// all-day, `YYYY-MM-DD`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    /// timed, RFC 3339 UTC
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub legacy_time: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
}

/// The `until` bound: a calendar date, or an instant (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    Day(NaiveDate),
    At(chrono::DateTime<chrono::Utc>),
}

/// `YYYY-MM-DD`, or an RFC 3339 instant with its zone. A wall time with no
/// zone is refused: nothing here reads local time.
pub fn parse_until(s: &str) -> Option<Until> {
    let s = s.trim();
    if let Some(d) = parse_date(s).filter(|_| s.len() == 10) {
        return Some(Until::Day(d));
    }
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| Until::At(t.with_timezone(&chrono::Utc)))
}

/// Every open item with a deadline, across all days, soonest first. Pure:
/// reads `md` (relative phrases resolved against `today`), writes nothing.
pub fn due_items(md: &str, today: NaiveDate, until: Option<Until>) -> Vec<DueItem> {
    let mut out: Vec<(String, DueItem)> = Vec::new();
    for chunk in parse_doc(md, today) {
        let Chunk::Day { date, entries } = chunk else { continue };
        for (i, it) in items_of(&entries).into_iter().enumerate() {
            if it.mark != ' ' {
                continue;
            }
            let Some(due) = it.deadline.as_deref().and_then(crate::due::parse_stored) else { continue };
            let keep = match (until, due.instant()) {
                (None, _) => true,
                (Some(Until::Day(u)), _) => due.utc_date() <= u,
                (Some(Until::At(u)), Some(t)) => t <= u,
                (Some(Until::At(u)), None) => due.utc_date() <= u.date_naive(),
            };
            if keep {
                // an all-day date sorts at the start of its day
                let key = match due.instant() {
                    Some(t) => crate::due::rfc3339(t),
                    None => due.utc_date().to_string(),
                };
                out.push((key, DueItem {
                    date: date.clone(),
                    id: item_id(i, &it.text),
                    text: it.text.clone(),
                    deadline: due.day().map(|d| d.to_string()),
                    due_at: due.instant().map(crate::due::rfc3339),
                    legacy_time: due.is_legacy(),
                    note: it.note.clone(),
                    carried_from: it.carried_from.clone(),
                }));
            }
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1.date, &a.1.id).cmp(&(&b.0, &b.1.date, &b.1.id)));
    out.into_iter().map(|(_, d)| d).collect()
}

/// Markdown after a write plus a phrase warning for the caller to surface.
pub type Written = (String, Option<&'static str>);

/// Append `- [ ] text` to the day (section created in date order). A trailing
/// `due <when>` / `by <when>` phrase (or a legacy `⏰ YYYY-MM-DD`) becomes the
/// deadline; one that looks like a date but does not parse is kept as typed
/// and reported. A typed time is wall time at `zone`; with no zone it is
/// refused (the server does not guess one).
pub fn add_item(md: &str, date: &str, today: &str, zone: Option<FixedOffset>, text: &str) -> Result<Written, String> {
    let tk = typed_tokens(text, today, zone)?;
    let mut chunks = parse_doc(md, today_date(today));
    let i = ensure_day(&mut chunks, date);
    touch_day(&mut chunks, i);
    entries_mut(&mut chunks, i).push(Entry::Item(ItemRec {
        mark: ' ',
        text: tk.text,
        deadline: tk.deadline,
        carried_from: None,
        note: None,
        raw: None,
    }));
    Ok((render_doc(&chunks), tk.warning))
}

/// A line the client typed: phrase resolved, a time only with a zone.
fn typed_tokens(text: &str, today: &str, zone: Option<FixedOffset>) -> Result<Tokens, String> {
    let tk = split_tokens(text.lines().next().unwrap_or(""), today_date(today), zone);
    if tk.text.is_empty() {
        return Err("empty to-do".into());
    }
    if zone.is_none() && tk.deadline.as_deref().and_then(crate::due::parse_stored).is_some_and(|d| d.is_legacy()) {
        return Err(NEED_OFFSET.into());
    }
    Ok(tk)
}

const NEED_OFFSET: &str = "a deadline with a time needs the client's utc_offset (e.g. +01:00)";

pub fn toggle_item(md: &str, date: &str, today: &str, id: Option<&str>, text: Option<&str>, done: bool) -> Result<String, String> {
    with_item(md, date, today, id, text, |it| it.mark = if done { 'x' } else { ' ' })
}

/// Replace the text; a phrase in it sets the deadline, none keeps the old one.
pub fn edit_item(md: &str, date: &str, today: &str, zone: Option<FixedOffset>, id: &str, text: &str) -> Result<Written, String> {
    let tk = typed_tokens(text, today, zone)?;
    let md = with_item(md, date, today, Some(id), None, |it| {
        it.text = tk.text;
        if tk.deadline.is_some() {
            it.deadline = tk.deadline;
        }
    })?;
    Ok((md, tk.warning))
}

/// Set the deadline: `deadline` (`YYYY-MM-DD`) makes it all-day, `due_at`
/// (RFC 3339 with a zone) timed — stored as UTC; neither clears it.
pub fn set_deadline(md: &str, date: &str, today: &str, id: &str, deadline: Option<&str>, due_at: Option<&str>) -> Result<String, String> {
    let due = match (due_at, deadline) {
        (Some(t), _) => Some(Due::At(
            crate::due::parse_instant(t).ok_or_else(|| format!("due_at must be RFC 3339 with a zone, got {t:?}"))?,
        )),
        (None, Some(d)) => Some(Due::Day(
            parse_date(d).filter(|_| d.len() == 10).ok_or_else(|| format!("deadline must be YYYY-MM-DD (use due_at for a time), got {d:?}"))?,
        )),
        (None, None) => None,
    };
    with_item(md, date, today, Some(id), None, |it| it.deadline = due.map(|d| d.stored()))
}

pub fn set_note(md: &str, date: &str, today: &str, id: &str, note: &str) -> Result<String, String> {
    let note = note.trim_end();
    // a blank line would split the block: collapse runs of blank lines
    let cleaned: Vec<&str> = note.lines().map(str::trim_end).collect();
    let mut kept: Vec<&str> = Vec::new();
    for l in cleaned {
        if l.trim().is_empty() {
            continue;
        }
        kept.push(l);
    }
    let note = (!kept.is_empty()).then(|| kept.join("\n"));
    with_item(md, date, today, Some(id), None, |it| it.note = note)
}

pub fn remove_item(md: &str, date: &str, today: &str, id: &str) -> Result<String, String> {
    let mut chunks = parse_doc(md, today_date(today));
    let i = day_index(&chunks, date).ok_or_else(|| format!("no to-do list for {date}"))?;
    touch_day(&mut chunks, i);
    let entries = entries_mut(&mut chunks, i);
    let p = locate(entries, id)?;
    entries.remove(p);
    Ok(render_doc(&chunks))
}

/// Move the item block under `## to_date` (created in date order), last in
/// that day. Mark, deadline, note and stamp travel with it.
pub fn move_item(md: &str, date: &str, today: &str, id: &str, to_date: &str) -> Result<String, String> {
    if !is_date(to_date) {
        return Err(format!("to_date must be YYYY-MM-DD, got {to_date:?}"));
    }
    let mut chunks = parse_doc(md, today_date(today));
    let i = day_index(&chunks, date).ok_or_else(|| format!("no to-do list for {date}"))?;
    touch_day(&mut chunks, i);
    let entries = entries_mut(&mut chunks, i);
    let p = locate(entries, id)?;
    let entry = entries.remove(p);
    let j = ensure_day(&mut chunks, to_date);
    touch_day(&mut chunks, j);
    entries_mut(&mut chunks, j).push(entry);
    Ok(render_doc(&chunks))
}

/// Copy the open `[ ]` items of the newest day before `date` into `date`'s
/// section and mark the sources `[>]`. Returns the new markdown and how many
/// moved; unchanged markdown (and 0) when there is nothing to carry.
pub fn carry_forward(md: &str, date: &str, today: &str) -> (String, usize) {
    let mut chunks = parse_doc(md, today_date(today));
    let Some(src) = days(&chunks).into_iter().filter(|d| *d < date).max().map(str::to_string) else {
        return (md.to_string(), 0);
    };
    let si = day_index(&chunks, &src).expect("source day exists");
    let mut moved: Vec<ItemRec> = Vec::new();
    for e in entries_mut(&mut chunks, si) {
        if let Entry::Item(it) = e
            && it.mark == ' '
        {
            let mut copy = it.clone();
            copy.carried_from = Some(it.carried_from.clone().unwrap_or_else(|| src.clone()));
            copy.raw = None;
            moved.push(copy);
            it.mark = '>';
            it.raw = None;
        }
    }
    if moved.is_empty() {
        return (md.to_string(), 0);
    }
    let n = moved.len();
    let ti = ensure_day(&mut chunks, date);
    touch_day(&mut chunks, ti);
    entries_mut(&mut chunks, ti).extend(moved.into_iter().map(Entry::Item));
    (render_doc(&chunks), n)
}

/// What the client says about its own clock (query or body).
#[derive(Debug, Default, Clone, Deserialize)]
struct ClientClock {
    #[serde(default)]
    today: Option<String>,
    #[serde(default)]
    utc_offset: Option<String>,
}

/// The client's date and zone, validated. `today` is required in SERVER mode
/// (`server`: the request carried a token); LOCAL mode falls back to the UTC
/// date. A missing offset stays None: only a typed time needs it.
fn client(c: &ClientClock, server: bool) -> Result<(String, Option<FixedOffset>), String> {
    let today = match c.today.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) if is_date(t) && parse_date(t).is_some() => t.to_string(),
        Some(t) => return Err(format!("today must be YYYY-MM-DD, got {t:?}")),
        None if server => return Err("today (the client's YYYY-MM-DD) is required".into()),
        None => chrono::Utc::now().date_naive().to_string(),
    };
    let zone = match c.utc_offset.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) => Some(crate::due::parse_offset(o).ok_or_else(|| format!("utc_offset must be like +01:00, got {o:?}"))?),
        None => None,
    };
    Ok((today, zone))
}

/// SERVER mode: the request came through `auth::require_auth` with a token.
type Server = Option<axum::Extension<crate::auth::Authenticated>>;

/// Carry only into today (or later) and only when no newer day exists: a
/// read of yesterday must never move items into yesterday.
fn should_carry(md: &str, date: &str, today: &str) -> bool {
    if date < today {
        return false;
    }
    !days(&parse_doc(md, today_date(today))).iter().any(|d| *d > date)
}

/* ---------- the doc ---------- */

/// The root doc titled To-do, created (by the human) when absent.
pub fn find_or_create_todo(s: &mut SqliteStore, human: Uuid) -> grimoire_store::Result<Doc> {
    if let Some(d) = s
        .list_docs()?
        .into_iter()
        .find(|d| d.parent_id.is_none() && d.title == TODO_TITLE)
    {
        return Ok(d);
    }
    s.create_doc(TODO_TITLE, None, human)
}

/// Save `new_md` over the doc as the human at the current epoch (no-op when
/// the text is unchanged). Returns the epoch afterwards.
fn save(s: &mut SqliteStore, doc: Uuid, human: Uuid, new_md: &str) -> Result<i64, String> {
    let tree = s.read_doc(doc).map_err(|e| e.to_string())?;
    let ops = grimoire_store::mddiff::markdown_to_ops_from(&tree.roots, new_md, "todo");
    if ops.is_empty() {
        return Ok(tree.doc.current_epoch);
    }
    let out = s
        .propose(doc, tree.doc.current_epoch, human, ops)
        .map_err(|e| e.to_string())?;
    if let Some(v) = out.verdicts.iter().find(|v| !v.applied) {
        return Err(format!("to-do write did not apply: {}", v.note));
    }
    Ok(out.epoch)
}

fn day_json(s: &SqliteStore, doc: &Doc, date: &str, today: &str, carried: usize, warning: Option<&str>) -> Result<Value, String> {
    let md = grimoire_store::export::export_doc(s, doc.id).map_err(|e| e.to_string())?;
    let tree = s.read_doc(doc.id).map_err(|e| e.to_string())?;
    let mut v = json!({
        "doc_id": doc.id,
        "date": date,
        "today": today,
        "items": parse_day(&md, date, today),
        "carried": carried,
        "prev_date": prev_day(&md, date),
        "epoch": tree.doc.current_epoch,
    });
    if let Some(w) = warning {
        v["warning"] = json!(w);
    }
    Ok(v)
}

fn check_date(d: &str) -> Result<(), String> {
    if is_date(d) { Ok(()) } else { Err(format!("date must be YYYY-MM-DD, got {d:?}")) }
}

/// Shared write shape: read the doc's markdown, transform (given today's
/// date for phrase resolution), save, answer with the day plus any phrase
/// warning.
async fn mutate(
    st: ApiState,
    server: bool,
    clock: ClientClock,
    date: String,
    f: impl FnOnce(&str, &str, Option<FixedOffset>) -> Result<Written, String> + Send + 'static,
) -> Json<Value> {
    if let Err(m) = check_date(&date) {
        return Json(json!({"error": m}));
    }
    let (today, zone) = match client(&clock, server) {
        Ok(c) => c,
        Err(m) => return Json(json!({"error": m})),
    };
    let human = st.human;
    let hot = st.hot.clone();
    with_store(&st.store, move |s| {
        let doc = match find_or_create_todo(s, human) {
            Ok(d) => d,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        if let Err(m) = hot.assert_cold(doc.id) {
            return Json(json!({"error": m}));
        }
        let md = match grimoire_store::export::export_doc(&*s, doc.id) {
            Ok(m) => m,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let (new_md, warning) = match f(&md, &today, zone) {
            Ok(m) => m,
            Err(e) => return Json(json!({"error": e})),
        };
        if let Err(e) = save(s, doc.id, human, &new_md) {
            return Json(json!({"error": e}));
        }
        match day_json(s, &doc, &date, &today, 0, warning) {
            Ok(v) => Json(v),
            Err(e) => Json(json!({"error": e})),
        }
    })
    .await
}

#[derive(Deserialize)]
struct DayQuery {
    date: Option<String>,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn get_day(State(st): State<ApiState>, server: Server, Query(q): Query<DayQuery>) -> Json<Value> {
    let today = match client(&q.clock, server.is_some()) {
        Ok((t, _)) => t,
        Err(m) => return Json(json!({"error": m})),
    };
    let date = q.date.unwrap_or_else(|| today.clone());
    if let Err(m) = check_date(&date) {
        return Json(json!({"error": m}));
    }
    let human = st.human;
    let hot = st.hot.clone();
    with_store(&st.store, move |s| {
        let doc = match find_or_create_todo(s, human) {
            Ok(d) => d,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let md = match grimoire_store::export::export_doc(&*s, doc.id) {
            Ok(m) => m,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let mut carried = 0;
        // a hot doc freezes writes: read only, carry next time
        if should_carry(&md, &date, &today) && hot.assert_cold(doc.id).is_ok() {
            let (new_md, n) = carry_forward(&md, &date, &today);
            if n > 0 {
                if let Err(e) = save(s, doc.id, human, &new_md) {
                    return Json(json!({"error": e}));
                }
                carried = n;
            }
        }
        match day_json(s, &doc, &date, &today, carried, None) {
            Ok(v) => Json(v),
            Err(e) => Json(json!({"error": e})),
        }
    })
    .await
}

#[derive(Deserialize)]
struct AddReq {
    date: String,
    text: String,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn add(State(st): State<ApiState>, server: Server, Json(req): Json<AddReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, zone| add_item(md, &date, today, zone, &req.text)).await
}

#[derive(Deserialize)]
struct DueQuery {
    until: Option<String>,
    #[serde(flatten)]
    clock: ClientClock,
}

/// Read-only across days: no carry-forward, and an absent To-do doc stays
/// absent (find, never create).
async fn due(State(st): State<ApiState>, Query(q): Query<DueQuery>) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let fail = |code: StatusCode, e: String| (code, Json(json!({"error": e}))).into_response();
    // `today` only resolves an agent's relative phrase still in the doc;
    // optional here in both modes (UTC date when absent)
    let today = match client(&q.clock, false) {
        Ok((t, _)) => today_date(&t),
        Err(m) => return fail(StatusCode::BAD_REQUEST, m),
    };
    let until = match q.until.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => match parse_until(u) {
            Some(x) => Some(x),
            None => return fail(StatusCode::BAD_REQUEST, format!("until must be YYYY-MM-DD or RFC 3339, got {u:?}")),
        },
        None => None,
    };
    let until_echo = q.until.clone();
    with_store(&st.store, move |s| {
        let now = chrono::Utc::now();
        let doc = match s.list_docs() {
            Ok(docs) => docs.into_iter().find(|d| d.parent_id.is_none() && d.title == TODO_TITLE),
            Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        let (doc_id, epoch, items) = match doc {
            Some(d) => match grimoire_store::export::export_doc(&*s, d.id) {
                Ok(md) => (Some(d.id.to_string()), d.current_epoch, due_items(&md, today, until)),
                Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            },
            None => (None, 0, Vec::new()),
        };
        Json(json!({
            "doc_id": doc_id,
            "epoch": epoch,
            "now": crate::due::rfc3339(now),
            "until": until_echo,
            "default_alert_time": crate::due::DEFAULT_ALERT_TIME,
            "items": items,
        }))
        .into_response()
    })
    .await
}

#[derive(Deserialize)]
struct ParseQuery {
    #[serde(default)]
    text: String,
    #[serde(flatten)]
    clock: ClientClock,
}

/// The UI's live hint while typing: what the daemon would make of `text`,
/// read against the client's `today` and `utc_offset`.
async fn parse(server: Server, Query(q): Query<ParseQuery>) -> Json<Value> {
    let (today, zone) = match client(&q.clock, server.is_some()) {
        Ok(c) => c,
        Err(m) => return Json(json!({"error": m})),
    };
    let tk = split_tokens(q.text.lines().next().unwrap_or(""), today_date(&today), zone);
    let due = tk.deadline.as_deref().and_then(crate::due::parse_stored);
    let mut v = json!({
        "text": tk.text,
        "deadline": due.and_then(|d| d.day()).map(|d| d.to_string()),
        "due_at": due.and_then(|d| d.instant()).map(crate::due::rfc3339),
    });
    if let Some(w) = tk.warning {
        v["warning"] = json!(w);
    } else if zone.is_none() && due.is_some_and(|d| d.is_legacy()) {
        v["warning"] = json!(NEED_OFFSET);
        v["due_at"] = Value::Null;
    }
    Json(v)
}

#[derive(Deserialize)]
struct ToggleReq {
    date: String,
    item_id: Option<String>,
    text: Option<String>,
    done: bool,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn toggle(State(st): State<ApiState>, server: Server, Json(req): Json<ToggleReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, _| {
        toggle_item(md, &date, today, req.item_id.as_deref(), req.text.as_deref(), req.done).map(|m| (m, None))
    })
    .await
}

#[derive(Deserialize)]
struct EditReq {
    date: String,
    item_id: String,
    text: String,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn edit(State(st): State<ApiState>, server: Server, Json(req): Json<EditReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, zone| {
        edit_item(md, &date, today, zone, &req.item_id, &req.text)
    })
    .await
}

#[derive(Deserialize)]
struct RemoveReq {
    date: String,
    item_id: String,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn remove(State(st): State<ApiState>, server: Server, Json(req): Json<RemoveReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, _| remove_item(md, &date, today, &req.item_id).map(|m| (m, None))).await
}

#[derive(Deserialize)]
struct MoveReq {
    date: String,
    item_id: String,
    to_date: String,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn mv(State(st): State<ApiState>, server: Server, Json(req): Json<MoveReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, _| move_item(md, &date, today, &req.item_id, &req.to_date).map(|m| (m, None))).await
}

#[derive(Deserialize)]
struct DeadlineReq {
    date: String,
    item_id: String,
    /// all-day `YYYY-MM-DD`
    #[serde(default)]
    deadline: Option<String>,
    /// timed: RFC 3339 with a zone (stored as UTC); wins over `deadline`
    #[serde(default)]
    due_at: Option<String>,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn deadline(State(st): State<ApiState>, server: Server, Json(req): Json<DeadlineReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, _| {
        let blank = |x: &Option<String>| x.as_deref().map(str::trim).filter(|d| !d.is_empty()).map(str::to_string);
        let (dl, at) = (blank(&req.deadline), blank(&req.due_at));
        set_deadline(md, &date, today, &req.item_id, dl.as_deref(), at.as_deref()).map(|m| (m, None))
    })
    .await
}

#[derive(Deserialize)]
struct NoteReq {
    date: String,
    item_id: String,
    #[serde(default)]
    note: String,
    #[serde(flatten)]
    clock: ClientClock,
}

async fn note(State(st): State<ApiState>, server: Server, Json(req): Json<NoteReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, server.is_some(), req.clock, req.date, move |md, today, _| set_note(md, &date, today, &req.item_id, &req.note).map(|m| (m, None))).await
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/todo", get(get_day).post(add))
        .route("/api/todo/parse", get(parse))
        .route("/api/todo/due", get(due))
        .route("/api/todo/toggle", post(toggle))
        .route("/api/todo/edit", post(edit))
        .route("/api/todo/remove", post(remove))
        .route("/api/todo/move", post(mv))
        .route("/api/todo/deadline", post(deadline))
        .route("/api/todo/note", post(note))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thursday 10 September 2026
    const T: &str = "2026-09-10";
    const DOC: &str = "## 2026-09-09\n\n- [ ] call the bank\n\n- [x] ship 0.8.0 · due 2026-09-12\n  needs Tom's sign-off after the local trial\n  release.sh --publish, then the hub box\n\n- [ ] rotate the token · due 2026-09-08 (carried from 2026-09-08)\n\n## 2026-09-10\n\n- [ ] write the report · due 2026-09-11\n";

    fn texts(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.text.as_str()).collect()
    }

    /// A client at UTC+0 (typed times then read as written).
    fn utc() -> Option<FixedOffset> {
        FixedOffset::east_opt(0)
    }

    fn add(md: &str, date: &str, text: &str) -> Result<String, String> {
        add_item(md, date, T, utc(), text).map(|(m, _)| m)
    }

    #[test]
    fn parses_items_with_deadline_note_and_stamp() {
        let items = parse_day(DOC, "2026-09-09", T);
        assert_eq!(texts(&items), ["call the bank", "ship 0.8.0", "rotate the token"]);
        assert_eq!(items[0].id, item_id(0, "call the bank"));
        assert!(items[0].deadline.is_none() && items[0].note.is_none());
        assert!(items[1].done);
        assert_eq!(items[1].deadline.as_deref(), Some("2026-09-12"));
        assert_eq!(
            items[1].note.as_deref(),
            Some("needs Tom's sign-off after the local trial\nrelease.sh --publish, then the hub box")
        );
        assert!(!items[1].due_soon, "done items never nag");
        assert_eq!(items[2].carried_from.as_deref(), Some("2026-09-08"));
        assert_eq!(items[2].deadline.as_deref(), Some("2026-09-08"));
        assert!(!items[2].due_soon, "past: not soon (overdue is the client's call)");
        let today = parse_day(DOC, T, T);
        assert!(today[0].due_soon);
        assert!(parse_day(DOC, "2026-09-11", T).is_empty());
        assert_eq!(prev_day(DOC, T).as_deref(), Some("2026-09-09"));
        assert_eq!(prev_day(DOC, "2026-09-09"), None);
    }

    #[test]
    fn the_canonical_form_round_trips_byte_for_byte() {
        assert_eq!(render_doc(&parse_doc(DOC, today_date(T))), DOC);
        // an agent's contiguous list and a preamble survive a rewrite
        let loose = "# To-do\n\nintro text\n\n## 2026-09-09\n- [ ] a\n- [ ] b due 2026-09-20\nfree note under the day\n";
        let items = parse_day(loose, "2026-09-09", T);
        assert_eq!(texts(&items), ["a", "b"]);
        let md = toggle_item(loose, "2026-09-09", T, Some(&items[0].id), None, true).unwrap();
        assert_eq!(md, "# To-do\n\nintro text\n\n## 2026-09-09\n\n- [x] a\n\n- [ ] b · due 2026-09-20\n  free note under the day\n");
    }

    #[test]
    fn legacy_clock_token_is_read_and_rewritten_on_the_next_write_of_its_day() {
        let old = "## 2026-09-09\n\n- [ ] old ⏰ 2026-09-12 (carried from 2026-09-08)\n\n## 2026-09-10\n\n- [ ] also old ⏰ 2026-09-13\n";
        let items = parse_day(old, "2026-09-09", T);
        assert_eq!(items[0].text, "old");
        assert_eq!(items[0].deadline.as_deref(), Some("2026-09-12"));
        assert_eq!(items[0].carried_from.as_deref(), Some("2026-09-08"));
        // a write to the 9th rewrites the 9th only; the 10th is left byte for byte
        let md = toggle_item(old, "2026-09-09", T, Some(&items[0].id), None, true).unwrap();
        assert_eq!(md, "## 2026-09-09\n\n- [x] old · due 2026-09-12 (carried from 2026-09-08)\n\n## 2026-09-10\n\n- [ ] also old ⏰ 2026-09-13\n");
        // the serializer itself never emits the clock: touch every day and render
        let mut chunks = parse_doc(&md, today_date(T));
        for i in 0..chunks.len() {
            if matches!(chunks[i], Chunk::Day { .. }) {
                touch_day(&mut chunks, i);
            }
        }
        let all = render_doc(&chunks);
        assert!(!all.contains(CLOCK), "{all}");
        assert!(all.contains("- [ ] also old · due 2026-09-13\n"));
        // adding a legacy token still works and is stored in the new form
        let md = add(&md, "2026-09-09", "typed ⏰ 2026-09-14").unwrap();
        assert!(md.contains("\n- [ ] typed · due 2026-09-14\n"), "{md}");
    }

    #[test]
    fn an_agents_typed_phrase_is_read_now_and_normalised_on_the_next_write_of_its_day() {
        // T is a Thursday: `fri` is the 11th, `12/9` the 12th
        let md = "## 2026-09-09\n\n- [ ] untouched due fri\n\n## 2026-09-10\n- [ ] pay rent by 12/9\n- [ ] read the due diligence doc\n- [ ] typo due 31/2\n";
        let items = parse_day(md, T, T);
        assert_eq!(texts(&items), ["pay rent", "read the due diligence doc", "typo due 31/2"]);
        assert_eq!(items[0].deadline.as_deref(), Some("2026-09-12"));
        assert!(items[1].deadline.is_none());
        assert!(items[2].deadline.is_none(), "an unreadable phrase is plain text on read");
        let other = parse_day(md, "2026-09-09", T);
        assert_eq!(other[0].text, "untouched");
        assert_eq!(other[0].deadline.as_deref(), Some("2026-09-11"));
        // a write to the 10th canonicalises the 10th; the 9th keeps `due fri`
        let out = toggle_item(md, T, T, Some(&items[1].id), None, true).unwrap();
        assert_eq!(
            out,
            "## 2026-09-09\n\n- [ ] untouched due fri\n\n## 2026-09-10\n\n- [ ] pay rent · due 2026-09-12\n\n- [x] read the due diligence doc\n\n- [ ] typo due 31/2\n"
        );
    }

    #[test]
    fn add_creates_days_in_date_order_and_reads_a_due_phrase() {
        let md = add("", T, "  first  ").unwrap();
        assert_eq!(md, "## 2026-09-10\n\n- [ ] first\n");
        let md = add(&md, T, "second due 2026-09-14").unwrap();
        assert_eq!(md, "## 2026-09-10\n\n- [ ] first\n\n- [ ] second · due 2026-09-14\n");
        let md = add(&md, T, "third by mon").unwrap();
        assert!(md.ends_with("- [ ] third · due 2026-09-14\n"), "{md}");
        let md = add(&md, T, "fourth · due 12 sep").unwrap();
        assert!(md.ends_with("- [ ] fourth · due 2026-09-12\n"), "{md}");
        let md = add(&md, T, "fifth due in 2 weeks").unwrap();
        assert!(md.ends_with("- [ ] fifth · due 2026-09-24\n"), "{md}");
        let md = add(&md, T, "sixth due Today").unwrap();
        assert!(md.ends_with("- [ ] sixth · due 2026-09-10\n"), "{md}");
        let md = add(&md, T, "seventh due thu").unwrap();
        assert!(md.ends_with("- [ ] seventh · due 2026-09-10\n"), "today is Thursday: {md}");
        let md = add(&md, T, "eighth due next thu").unwrap();
        assert!(md.ends_with("- [ ] eighth · due 2026-09-17\n"), "{md}");
        // a phrase that looks like a date but is not: text kept, warning returned
        let (md, warn) = add_item(&md, T, T, utc(), "rent due 31/2").unwrap();
        assert_eq!(warn, Some(crate::due::WARNING));
        assert!(md.ends_with("- [ ] rent due 31/2\n"), "{md}");
        // a mid-sentence due is text
        let (md, warn) = add_item(&md, T, T, utc(), "read the due diligence doc").unwrap();
        assert!(warn.is_none());
        assert!(md.ends_with("- [ ] read the due diligence doc\n"), "{md}");
        let md = add(&md, "2026-09-12", "later").unwrap();
        let md = add(&md, "2026-09-11", "between").unwrap();
        assert_eq!(days(&parse_doc(&md, today_date(T))), ["2026-09-10", "2026-09-11", "2026-09-12"]);
        let md = add(&md, "2026-09-01", "early").unwrap();
        assert_eq!(days(&parse_doc(&md, today_date(T))), ["2026-09-01", "2026-09-10", "2026-09-11", "2026-09-12"]);
        assert!(add("", T, "   ").is_err());
        assert!(add("", T, "due tomorrow").is_err(), "a bare deadline is not a to-do");
        assert!(add("", T, "⏰ 2026-09-14").is_err(), "nor is a bare legacy token");
    }

    #[test]
    fn toggle_edit_deadline_note_remove() {
        let items = parse_day(DOC, "2026-09-09", T);
        let md = toggle_item(DOC, "2026-09-09", T, Some(&items[0].id), None, true).unwrap();
        assert!(md.contains("\n- [x] call the bank\n"));
        let md = toggle_item(&md, "2026-09-09", T, None, Some("ship 0.8.0"), false).unwrap();
        assert!(md.contains("- [ ] ship 0.8.0 · due 2026-09-12\n  needs Tom's"));
        // edit keeps deadline, note and stamp
        let (md, _) = edit_item(&md, "2026-09-09", T, utc(), &items[2].id, "rotate ALL tokens").unwrap();
        assert!(md.contains("- [ ] rotate ALL tokens · due 2026-09-08 (carried from 2026-09-08)\n"));
        // an edit with a phrase moves the deadline
        let items = parse_day(&md, "2026-09-09", T);
        let (md, warn) = edit_item(&md, "2026-09-09", T, utc(), &items[2].id, "rotate ALL tokens by sat").unwrap();
        assert!(warn.is_none());
        assert!(md.contains("- [ ] rotate ALL tokens · due 2026-09-12 (carried from 2026-09-08)\n"), "{md}");
        // an edit with an unreadable phrase keeps the text as typed and the old deadline
        let (md, warn) = edit_item(&md, "2026-09-09", T, utc(), &items[2].id, "rotate ALL tokens by 32/9").unwrap();
        assert_eq!(warn, Some(crate::due::WARNING));
        assert!(md.contains("- [ ] rotate ALL tokens by 32/9 · due 2026-09-12 (carried from 2026-09-08)\n"), "{md}");
        let (md, _) = edit_item(&md, "2026-09-09", T, utc(), &parse_day(&md, "2026-09-09", T)[2].id, "rotate ALL tokens").unwrap();
        let items = parse_day(&md, "2026-09-09", T);
        // deadline set / clear
        let md = set_deadline(&md, "2026-09-09", T, &items[0].id, Some("2026-09-30"), None).unwrap();
        assert!(md.contains("- [x] call the bank · due 2026-09-30\n"));
        let md = set_deadline(&md, "2026-09-09", T, &items[2].id, None, None).unwrap();
        assert!(md.contains("- [ ] rotate ALL tokens (carried from 2026-09-08)\n"));
        assert!(set_deadline(&md, "2026-09-09", T, &items[0].id, Some("soon"), None).is_err());
        // note set (blank lines inside collapse — they would split the block) / clear
        let md = set_note(&md, "2026-09-09", T, &items[0].id, "ask for\n\nthe statement  \n").unwrap();
        assert!(md.contains("- [x] call the bank · due 2026-09-30\n  ask for\n  the statement\n\n- [ ] ship"));
        assert_eq!(parse_day(&md, "2026-09-09", T)[0].note.as_deref(), Some("ask for\nthe statement"));
        let md = set_note(&md, "2026-09-09", T, &items[1].id, "").unwrap();
        assert!(parse_day(&md, "2026-09-09", T)[1].note.is_none());
        // remove
        let md = remove_item(&md, "2026-09-09", T, &items[0].id).unwrap();
        assert_eq!(texts(&parse_day(&md, "2026-09-09", T)), ["ship 0.8.0", "rotate ALL tokens"]);
        assert_eq!(texts(&parse_day(&md, T, T)), ["write the report"], "other days untouched");
        assert!(toggle_item(DOC, "2026-09-09", T, Some("9-00000000"), None, true).is_err());
        assert!(toggle_item(DOC, "2026-09-12", T, Some(&items[0].id), None, true).is_err());
        assert!(!md.contains(CLOCK));
    }

    #[test]
    fn id_falls_back_to_the_hash_when_the_list_shifted() {
        let items = parse_day(DOC, "2026-09-09", T);
        let shifted = DOC.replace("- [ ] call the bank", "- [ ] new agent item\n\n- [ ] call the bank");
        let md = toggle_item(&shifted, "2026-09-09", T, Some(&items[0].id), None, true).unwrap();
        assert!(md.contains("- [x] call the bank\n"));
        assert!(md.contains("- [ ] new agent item\n"));
    }

    #[test]
    fn move_crosses_sections_creating_the_heading_in_order() {
        let items = parse_day(DOC, "2026-09-09", T);
        // into an existing day: lands last, everything travels
        let md = move_item(DOC, "2026-09-09", T, &items[1].id, T).unwrap();
        assert_eq!(texts(&parse_day(&md, "2026-09-09", T)), ["call the bank", "rotate the token"]);
        let today = parse_day(&md, T, T);
        assert_eq!(texts(&today), ["write the report", "ship 0.8.0"]);
        assert!(today[1].done);
        assert_eq!(today[1].deadline.as_deref(), Some("2026-09-12"));
        assert!(today[1].note.as_deref().unwrap().starts_with("needs Tom"));
        // into a new day between the two: heading created in date order
        let items = parse_day(&md, T, T);
        let md = move_item(&md, T, T, &items[0].id, "2026-09-15").unwrap();
        let md = move_item(&md, "2026-09-09", T, &parse_day(&md, "2026-09-09", T)[0].id, "2026-09-11").unwrap();
        assert_eq!(days(&parse_doc(&md, today_date(T))), ["2026-09-09", "2026-09-10", "2026-09-11", "2026-09-15"]);
        assert_eq!(texts(&parse_day(&md, "2026-09-11", T)), ["call the bank"]);
        assert_eq!(texts(&parse_day(&md, "2026-09-15", T)), ["write the report"]);
        assert!(move_item(&md, T, T, "0-deadbeef", "2026-09-15").is_err());
        assert!(move_item(&md, T, T, &items[1].id, "tomorrow").is_err());
    }

    #[test]
    fn carry_forward_moves_open_items_once_with_deadlines_intact() {
        let (md, n) = carry_forward(DOC, T, T);
        assert_eq!(n, 2);
        let today = parse_day(&md, T, T);
        assert_eq!(texts(&today), ["write the report", "call the bank", "rotate the token"]);
        assert_eq!(today[1].carried_from.as_deref(), Some("2026-09-09"));
        // the original origin and the (past) deadline are kept
        assert_eq!(today[2].carried_from.as_deref(), Some("2026-09-08"));
        assert_eq!(today[2].deadline.as_deref(), Some("2026-09-08"));
        let src = parse_day(&md, "2026-09-09", T);
        assert!(src[0].carried && !src[0].done);
        assert!(src[1].done && !src[1].carried);
        assert!(src[2].carried);
        // idempotent
        let (again, n2) = carry_forward(&md, T, T);
        assert_eq!(n2, 0);
        assert_eq!(again, md);
        // a fresh later day gets a section with the notes travelling too
        let md2 = set_note(&md, T, T, &today[0].id, "draft in Grimoire").unwrap();
        let (md3, n3) = carry_forward(&md2, "2026-09-11", "2026-09-11");
        assert_eq!(n3, 3);
        let next = parse_day(&md3, "2026-09-11", "2026-09-11");
        assert_eq!(next[0].note.as_deref(), Some("draft in Grimoire"));
        assert_eq!(next[0].carried_from.as_deref(), Some(T));
        assert_eq!(carry_forward(DOC, "2026-09-01", T).1, 0, "nothing before the first day");
    }

    #[test]
    fn carry_only_into_today_and_never_under_a_newer_day() {
        assert!(should_carry(DOC, T, T));
        assert!(should_carry(DOC, "2026-09-11", T));
        assert!(!should_carry(DOC, "2026-09-09", T), "reading yesterday");
        assert!(!should_carry(DOC, "2026-09-09", "2026-09-09"), "today but a newer day exists");
    }

    #[tokio::test]
    async fn routes_round_trip_through_one_root_doc_as_the_human() {
        use crate::home::testing::{app, call};
        let (app, human) = app();
        let a = call(&app, "POST", "/api/todo", Some(json!({"date": "2026-09-09", "text": "call the bank due 2026-09-09"}))).await;
        assert_eq!(a["items"][0]["text"], "call the bank", "{a}");
        assert_eq!(a["items"][0]["deadline"], "2026-09-09");
        assert!(a.get("warning").is_none());
        let doc_id = a["doc_id"].as_str().unwrap().to_string();
        call(&app, "POST", "/api/todo", Some(json!({"date": "2026-09-09", "text": "done thing"}))).await;
        let t = call(&app, "POST", "/api/todo/toggle", Some(json!({"date": "2026-09-09", "text": "done thing", "done": true}))).await;
        assert_eq!(t["items"][1]["done"], true, "{t}");
        // the doc is one block per item under the day heading, in the new form
        let tree = call(&app, "GET", &format!("/api/doc/{doc_id}"), None).await;
        let roots = tree["roots"].as_array().unwrap();
        assert_eq!(roots.len(), 1, "one day heading: {tree}");
        assert_eq!(roots[0]["block"]["content"], "## 2026-09-09");
        let kids = roots[0]["children"].as_array().unwrap();
        assert_eq!(kids.len(), 2, "two item blocks");
        assert_eq!(kids[0]["block"]["content"], "- [ ] call the bank · due 2026-09-09");
        // the client's today: carry-forward pulls the open one, once, deadline intact
        let today = "2026-09-10";
        let g = call(&app, "GET", &format!("/api/todo?date={today}&today={today}"), None).await;
        assert_eq!(g["carried"], 1, "{g}");
        assert_eq!(g["items"][0]["text"], "call the bank");
        assert_eq!(g["items"][0]["carried_from"], "2026-09-09");
        assert!(g["items"][0].get("overdue").is_none(), "clients compute overdue: {g}");
        assert_eq!(g["prev_date"], "2026-09-09");
        assert_eq!(g["today"], today);
        let g2 = call(&app, "GET", &format!("/api/todo?date={today}&today={today}"), None).await;
        assert_eq!(g2["carried"], 0);
        assert_eq!(g2["items"].as_array().unwrap().len(), 1);
        let y = call(&app, "GET", &format!("/api/todo?date=2026-09-09&today={today}"), None).await;
        assert_eq!(y["items"][0]["carried"], true, "{y}");
        assert_eq!(y["carried"], 0);
        // note, deadline, edit, move, remove by id
        let tid = g2["items"][0]["id"].as_str().unwrap().to_string();
        let n = call(&app, "POST", "/api/todo/note", Some(json!({"date": today, "item_id": tid, "note": "about the mortgage", "today": today}))).await;
        assert_eq!(n["items"][0]["note"], "about the mortgage", "{n}");
        let d = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": today, "item_id": tid, "deadline": null, "today": today}))).await;
        assert!(d["items"][0].get("deadline").is_none(), "{d}");
        let e = call(&app, "POST", "/api/todo/edit", Some(json!({"date": today, "item_id": tid, "text": "call the bank today", "today": today}))).await;
        assert_eq!(e["items"][0]["text"], "call the bank today");
        assert_eq!(e["items"][0]["note"], "about the mortgage");
        // an edit with a phrase sets the deadline relative to the CLIENT's today
        let tid = e["items"][0]["id"].as_str().unwrap().to_string();
        let e = call(&app, "POST", "/api/todo/edit", Some(json!({"date": today, "item_id": tid, "text": "call the bank today due tomorrow", "today": today}))).await;
        assert_eq!(e["items"][0]["text"], "call the bank today", "{e}");
        let tomorrow = "2026-09-11";
        assert_eq!(e["items"][0]["deadline"], tomorrow);
        assert_eq!(e["items"][0]["due_soon"], true);
        let tid = e["items"][0]["id"].as_str().unwrap().to_string();
        let m = call(&app, "POST", "/api/todo/move", Some(json!({"date": today, "item_id": tid, "to_date": "2099-01-01", "today": today}))).await;
        assert_eq!(m["items"].as_array().unwrap().len(), 0, "answers with the source day: {m}");
        let far = call(&app, "GET", &format!("/api/todo?date=2099-01-01&today={today}"), None).await;
        assert_eq!(far["items"][0]["text"], "call the bank today");
        let tid = far["items"][0]["id"].as_str().unwrap().to_string();
        let r = call(&app, "POST", "/api/todo/remove", Some(json!({"date": "2099-01-01", "item_id": tid}))).await;
        assert_eq!(r["items"].as_array().unwrap().len(), 0);
        // an unreadable phrase: kept as typed, warned about
        let w = call(&app, "POST", "/api/todo", Some(json!({"date": today, "text": "rent due 31/2", "today": today}))).await;
        assert_eq!(w["warning"], crate::due::WARNING, "{w}");
        let last = w["items"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["text"], "rent due 31/2");
        assert!(last.get("deadline").is_none());
        // the live hint
        let p = call(&app, "GET", &format!("/api/todo/parse?text=pay%20rent%20by%20tomorrow&today={today}"), None).await;
        assert_eq!(p["text"], "pay rent");
        assert_eq!(p["deadline"], tomorrow);
        assert!(p.get("warning").is_none());
        let p = call(&app, "GET", "/api/todo/parse?text=pay%20rent%20due%2031%2F2", None).await;
        assert_eq!(p["text"], "pay rent due 31/2");
        assert!(p["deadline"].is_null());
        assert_eq!(p["warning"], crate::due::WARNING);
        let p = call(&app, "GET", "/api/todo/parse?text=read%20the%20due%20diligence%20doc", None).await;
        assert_eq!(p["text"], "read the due diligence doc");
        assert!(p["deadline"].is_null() && p.get("warning").is_none());
        assert!(p["due_at"].is_null());
        // a time is the client's wall time at its offset: due_at in UTC, no date
        let q = format!("/api/todo/parse?text=pay%20rent%20by%20tomorrow%2009%3A30&today={today}&utc_offset=%2B01%3A00");
        let p = call(&app, "GET", &q, None).await;
        assert_eq!(p["text"], "pay rent");
        assert!(p["deadline"].is_null(), "{p}");
        assert_eq!(p["due_at"], "2026-09-11T08:30:00Z");
        // no offset: the time is not guessed
        let p = call(&app, "GET", &format!("/api/todo/parse?text=pay%20rent%20by%20tomorrow%2009%3A30&today={today}"), None).await;
        assert!(p["due_at"].is_null() && p["warning"].as_str().unwrap().contains("utc_offset"), "{p}");
        let p = call(&app, "GET", "/api/todo/parse?text=x&utc_offset=1", None).await;
        assert!(p["error"].as_str().unwrap().contains("utc_offset"), "{p}");
        // exactly one root To-do doc, created by the human, every write human
        let docs = call(&app, "GET", "/api/docs", None).await;
        let docs = docs.as_array().unwrap();
        assert_eq!(docs.iter().filter(|d| d["title"] == TODO_TITLE).count(), 1);
        let td = docs.iter().find(|d| d["id"] == doc_id).unwrap();
        assert!(td["parent_id"].is_null());
        assert_eq!(td["created_by"], human.to_string());
        let hist = call(&app, "GET", &format!("/api/doc/{doc_id}/history"), None).await;
        assert!(!hist.as_array().unwrap().is_empty());
        assert!(hist.as_array().unwrap().iter().all(|h| h["principal_kind"] == "human"), "{hist}");
        // bad input
        let e = call(&app, "GET", "/api/todo?date=yesterday", None).await;
        assert!(e["error"].is_string());
        let e = call(&app, "POST", "/api/todo", Some(json!({"date": today, "text": "  ", "today": today}))).await;
        assert!(e["error"].is_string());
        let e = call(&app, "GET", "/api/todo?today=10/09/2026", None).await;
        assert!(e["error"].as_str().unwrap().contains("today"), "{e}");
        let e = call(&app, "POST", "/api/todo", Some(json!({"date": today, "text": "call due fri 3pm", "today": today}))).await;
        assert!(e["error"].as_str().unwrap().contains("utc_offset"), "a typed time needs the offset: {e}");
    }

    #[test]
    fn a_typed_time_is_stored_as_a_utc_instant() {
        let dublin = crate::due::parse_offset("+01:00");
        let (md, _) = add_item("", T, T, dublin, "call the bank due fri 3pm").unwrap();
        assert_eq!(md, "## 2026-09-10\n\n- [ ] call the bank · due 2026-09-11T14:00Z\n");
        let items = parse_day(&md, T, T);
        assert_eq!(items[0].text, "call the bank");
        assert!(items[0].deadline.is_none(), "a timed item has no all-day date");
        assert_eq!(items[0].due_at.as_deref(), Some("2026-09-11T14:00:00Z"));
        assert!(!items[0].legacy_time && items[0].due_soon);
        // date-only stays a floating date
        let (md, _) = add_item(&md, T, T, dublin, "ship it by 12/10").unwrap();
        assert!(md.contains("- [ ] ship it · due 2026-10-12\n"), "{md}");
        let items = parse_day(&md, T, T);
        assert_eq!((items[1].deadline.as_deref(), items[1].due_at.as_deref()), (Some("2026-10-12"), None));
        // the stored instant re-reads identically and survives a rewrite of the day
        let md = toggle_item(&md, T, T, Some(&items[0].id), None, true).unwrap();
        assert!(md.contains("- [x] call the bank · due 2026-09-11T14:00Z\n"), "{md}");
        // an edit without a phrase keeps it; one with a phrase replaces it
        let (md, _) = edit_item(&md, T, T, dublin, &items[0].id, "call the bank about fees").unwrap();
        assert!(md.contains("- [x] call the bank about fees · due 2026-09-11T14:00Z\n"), "{md}");
        let id = parse_day(&md, T, T)[0].id.clone();
        let (md, _) = edit_item(&md, T, T, crate::due::parse_offset("-05:00"), &id, "call the bank about fees by 12/10 17:00").unwrap();
        assert!(md.contains("· due 2026-10-12T22:00Z\n"), "{md}");
        // no zone: a typed time is refused, a date is fine
        assert!(add_item(&md, T, T, None, "x due fri 3pm").unwrap_err().contains("utc_offset"));
        assert!(add_item(&md, T, T, None, "x due fri").is_ok());
        // set_deadline: due_at (any zone) → UTC; a date → all-day
        let id = parse_day(&md, T, T)[0].id.clone();
        let md = set_deadline(&md, T, T, &id, None, Some("2026-09-20T08:15:00+01:00")).unwrap();
        assert!(md.contains("· due 2026-09-20T07:15Z\n"), "{md}");
        let md = set_deadline(&md, T, T, &id, Some("2026-09-21"), None).unwrap();
        assert!(md.contains("· due 2026-09-21\n"), "{md}");
        assert!(set_deadline(&md, T, T, &id, None, Some("2026-09-20T08:15")).is_err(), "no zone, no guess");
        assert!(set_deadline(&md, T, T, &id, Some("2026-09-21 10:00"), None).is_err());
    }

    #[test]
    fn legacy_wall_times_are_read_as_utc_flagged_and_left_alone() {
        let old = "## 2026-09-10\n\n- [ ] call · due 2026-09-11 15:00\n\n- [ ] standup due tomorrow 9:45\n";
        let items = parse_day(old, T, T);
        assert_eq!(items[0].due_at.as_deref(), Some("2026-09-11T15:00:00Z"));
        assert!(items[0].legacy_time && items[0].deadline.is_none());
        assert_eq!((items[1].text.as_str(), items[1].due_at.as_deref(), items[1].legacy_time), ("standup", Some("2026-09-11T09:45:00Z"), true));
        // a write to the day keeps the legacy form (never silently rezoned)
        let md = toggle_item(old, T, T, Some(&items[0].id), None, true).unwrap();
        assert!(md.contains("- [x] call · due 2026-09-11 15:00\n"), "{md}");
        // the clock token stays date-only
        let items = parse_day("## 2026-09-10\n\n- [ ] old ⏰ 2026-09-12\n", T, T);
        assert_eq!((items[0].deadline.as_deref(), items[0].due_at.as_deref()), (Some("2026-09-12"), None));
    }

    fn utc_at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn due_lists_open_deadlined_items_across_days_soonest_first() {
        let md = "## 2026-09-08\n\n- [ ] old and late · due 2026-09-09\n\n- [>] moved on · due 2026-09-09\n\n## 2026-09-10\n\n- [ ] standup · due 2026-09-10T08:30Z\n  room 4\n\n- [ ] report · due 2026-09-10\n\n- [x] done already · due 2026-09-10T07:00Z\n\n- [ ] no deadline\n\n- [ ] later · due 2026-09-20 17:00 (carried from 2026-09-08)\n";
        let today = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        let all = due_items(md, today, None);
        let rows: Vec<(&str, Option<&str>, Option<&str>)> =
            all.iter().map(|i| (i.text.as_str(), i.deadline.as_deref(), i.due_at.as_deref())).collect();
        assert_eq!(
            rows,
            vec![
                ("old and late", Some("2026-09-09"), None),
                ("report", Some("2026-09-10"), None),
                ("standup", None, Some("2026-09-10T08:30:00Z")),
                ("later", None, Some("2026-09-20T17:00:00Z")),
            ],
            "open + deadlined only, soonest first, an all-day date at the start of its day"
        );
        let standup = &all[2];
        assert_eq!(standup.date, "2026-09-10");
        assert_eq!(standup.id, item_id(0, "standup"), "addressable with {{date, item_id}}");
        assert_eq!(standup.note.as_deref(), Some("room 4"));
        assert!(all[3].legacy_time);
        assert_eq!(all[3].carried_from.as_deref(), Some("2026-09-08"));
        // until a date: all-day on or before it, timed by UTC date
        let d = due_items(md, today, Some(Until::Day(today)));
        assert_eq!(d.len(), 3);
        // until an instant: timed at or before it, all-day on or before its UTC date
        let d = due_items(md, today, Some(Until::At(utc_at("2026-09-10T08:29:00Z"))));
        assert_eq!(d.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(), ["old and late", "report"]);
        let d = due_items(md, today, Some(Until::At(utc_at("2026-09-10T09:30:00+01:00"))));
        assert_eq!(d.len(), 3, "08:30Z is in");
        assert!(due_items("", today, None).is_empty());
    }

    #[test]
    fn until_is_a_date_or_an_instant_with_a_zone() {
        assert_eq!(parse_until("2026-09-10"), Some(Until::Day(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap())));
        assert_eq!(parse_until("2026-09-10T17:00:00Z"), Some(Until::At(utc_at("2026-09-10T17:00:00Z"))));
        assert_eq!(parse_until("2026-09-10T18:00:00+01:00"), Some(Until::At(utc_at("2026-09-10T17:00:00Z"))));
        for bad in ["tomorrow", "2026-13-01", "2026-09-10 17:00", "2026-09-10T17:00", ""] {
            assert_eq!(parse_until(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn due_route_is_read_only() {
        use crate::home::testing::{app, call};
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let (app, _) = app();
        // no To-do doc yet: the read must not create one
        let v = call(&app, "GET", "/api/todo/due", None).await;
        assert!(v["doc_id"].is_null() && v["items"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(v["default_alert_time"], "09:00");
        assert!(v["now"].as_str().unwrap().ends_with('Z'), "{v}");
        let docs = call(&app, "GET", "/api/docs", None).await;
        assert!(docs.as_array().unwrap().is_empty(), "no doc created: {docs}");
        // an open item on a PAST day: GET /api/todo for today would carry it
        let t = "2020-01-01";
        call(&app, "POST", "/api/todo", Some(json!({"date": t, "text": "pay the fine due 2020-01-02 10:00", "today": t, "utc_offset": "+00:00"}))).await;
        call(&app, "POST", "/api/todo", Some(json!({"date": t, "text": "renew due 2099-01-01", "today": t}))).await;
        let before = call(&app, "GET", "/api/changes", None).await["seq"].clone();
        let epoch = |docs: &Value| docs.as_array().unwrap().iter().find(|d| d["title"] == TODO_TITLE).unwrap()["current_epoch"].clone();
        let e0 = epoch(&call(&app, "GET", "/api/docs", None).await);
        let v = call(&app, "GET", "/api/todo/due", None).await;
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "{v}");
        assert_eq!(items[0]["text"], "pay the fine");
        assert_eq!(items[0]["date"], "2020-01-01");
        assert_eq!(items[0]["due_at"], "2020-01-02T10:00:00Z");
        assert!(items[0].get("deadline").is_none() && items[0].get("overdue").is_none());
        assert_eq!(items[1]["deadline"], "2099-01-01");
        assert!(items[1].get("due_at").is_none());
        let v = call(&app, "GET", "/api/todo/due?until=2098-12-31", None).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1);
        let v = call(&app, "GET", "/api/todo/due?until=2020-01-02T10:00:00Z", None).await;
        assert_eq!(v["items"].as_array().unwrap().len(), 1, "an instant AT the bound is in");
        let v = call(&app, "GET", "/api/todo/due?until=2020-01-02T10:59:00%2B01:00", None).await;
        assert!(v["items"].as_array().unwrap().is_empty(), "09:59Z is before it");
        // nothing moved: same epoch, same journal head
        assert_eq!(epoch(&call(&app, "GET", "/api/docs", None).await), e0);
        assert_eq!(call(&app, "GET", "/api/changes", None).await["seq"], before);
        // a bad bound (or a zoneless wall time) is a 400 with a JSON body
        for bad in ["soon", "2020-01-02T10:00"] {
            let res = app.clone().oneshot(Request::get(format!("/api/todo/due?until={bad}")).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[tokio::test]
    async fn the_deadline_route_sets_a_day_an_instant_or_clears() {
        use crate::home::testing::{app, call};
        let (app, _) = app();
        let d = "2020-01-01";
        let v = call(&app, "POST", "/api/todo", Some(json!({"date": d, "text": "x due 2020-01-05", "today": d}))).await;
        let id = v["items"][0]["id"].as_str().unwrap().to_string();
        let v = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": d, "item_id": id, "due_at": "2020-01-06T15:00:00+01:00", "today": d}))).await;
        assert_eq!((v["items"][0]["due_at"].clone(), v["items"][0].get("deadline").cloned()), (json!("2020-01-06T14:00:00Z"), None), "{v}");
        let v = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": d, "item_id": id, "deadline": "2020-01-07", "today": d}))).await;
        assert_eq!((v["items"][0]["deadline"].clone(), v["items"][0].get("due_at").cloned()), (json!("2020-01-07"), None), "{v}");
        let v = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": d, "item_id": id, "due_at": "2020-01-06 15:00", "today": d}))).await;
        assert!(v["error"].as_str().unwrap().contains("RFC 3339"), "{v}");
        let v = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": d, "item_id": id, "deadline": null, "today": d}))).await;
        assert!(v["items"][0].get("deadline").is_none() && v["items"][0].get("due_at").is_none(), "{v}");
    }

    /// SERVER mode (a token on the request): the client's `today` is required.
    #[tokio::test]
    async fn server_mode_requires_the_clients_today() {
        use crate::home::testing::app;
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let (app, human) = app();
        let who = crate::auth::Authenticated {
            user_id: Uuid::now_v7(),
            grant_id: Uuid::now_v7(),
            client_id: "dcr_x".into(),
            principal: "claude:taisce-ios".into(),
            owner_app: true,
            human,
        };
        let get = |uri: &str| {
            let mut r = Request::get(uri).body(Body::empty()).unwrap();
            r.extensions_mut().insert(who.clone());
            r
        };
        let body = |r: axum::response::Response| async move {
            serde_json::from_slice::<Value>(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap()
        };
        let v = body(app.clone().oneshot(get("/api/todo")).await.unwrap()).await;
        assert!(v["error"].as_str().unwrap().contains("today"), "{v}");
        let v = body(app.clone().oneshot(get("/api/todo/parse?text=x%20due%20fri")).await.unwrap()).await;
        assert!(v["error"].as_str().unwrap().contains("today"), "{v}");
        let v = body(app.clone().oneshot(get("/api/todo?today=2026-09-10")).await.unwrap()).await;
        assert_eq!(v["today"], "2026-09-10", "{v}");
        let mut r = Request::post("/api/todo")
            .header("content-type", "application/json")
            .body(Body::from(json!({"date": "2026-09-10", "text": "x"}).to_string()))
            .unwrap();
        r.extensions_mut().insert(who.clone());
        let v = body(app.clone().oneshot(r).await.unwrap()).await;
        assert!(v["error"].as_str().unwrap().contains("today"), "{v}");
    }
}
