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
//!   `due.rs` for the grammar; the daemon resolves it against its local date
//!   and stores the ISO form. The pre-0.8 `⏰ YYYY-MM-DD` token is still read
//!   and rewritten to the new form the next time its day is written.
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
//! Carry-forward: a `GET` for today (a date ≥ the daemon's local date, with no
//! newer day in the doc) copies the still-open `[ ]` items of the most recent
//! earlier day into today's section — deadline and note intact, stamped
//! `(carried from <original day>)` — and marks the sources `[>]`. A repeat GET
//! finds no `[ ]` there and copies nothing, so it is idempotent even when an
//! agent already started today's section; a read of a past day never moves
//! anything.
//!
//! Routes (all human-principal):
//! - `GET  /api/todo?date=YYYY-MM-DD` → `{doc_id, date, today, items:[{id, text,
//!   done, carried?, carried_from?, deadline?, note?, overdue, due_soon}],
//!   carried, prev_date, epoch}`
//! - `POST /api/todo {date, text}` add; a trailing `due <when>` / `by <when>`
//!   phrase becomes the deadline. One that looks like a date but does not
//!   parse leaves the text as typed and adds `warning: "couldn't read that
//!   date"` to the answer.
//! - `POST /api/todo/toggle {date, item_id | text, done}`
//! - `POST /api/todo/edit {date, item_id, text}` (same phrase handling; a text
//!   without a phrase keeps the item's deadline)
//! - `GET  /api/todo/parse?text=…` → `{text, deadline | null, warning?}` — the
//!   UI's live hint, so the rules live here only
//! - `POST /api/todo/remove {date, item_id}`
//! - `POST /api/todo/move {date, item_id, to_date}` (heading created in date order)
//! - `POST /api/todo/deadline {date, item_id, deadline | null}`
//! - `POST /api/todo/note {date, item_id, note}` (empty clears)

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub overdue: bool,
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

fn today_date(today: &str) -> NaiveDate {
    parse_date(today).unwrap_or_else(|| chrono::Local::now().date_naive())
}

/// Pull the trailing `(carried from D)` stamp, the legacy `⏰ D` token and a
/// trailing `due <when>` / `by <when>` phrase (resolved against `today`) out
/// of an item's first line.
fn split_tokens(raw: &str, today: NaiveDate) -> Tokens {
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
    let parsed = crate::due::split_due(&t, today);
    let text = if deadline.is_none() {
        deadline = parsed.deadline.map(|d| d.to_string());
        warning = parsed.warning;
        parsed.text
    } else {
        t.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    Tokens { text, deadline, carried_from: from, warning }
}

fn parse_item(block: &[&str], today: NaiveDate) -> Option<ItemRec> {
    let (mark, first) = split_item_line(block[0])?;
    let Tokens { text, deadline, carried_from, .. } = split_tokens(first, today);
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
    let (overdue, due_soon) = match (&it.deadline, parse_date(today)) {
        (Some(d), Some(t)) if open => match parse_date(d) {
            Some(dl) => {
                let diff = (dl - t).num_days();
                (diff < 0, (0..=DUE_SOON_DAYS).contains(&diff))
            }
            None => (false, false),
        },
        _ => (false, false),
    };
    Item {
        id: item_id(index, &it.text),
        text: it.text.clone(),
        done: it.mark == 'x',
        carried: it.mark == '>',
        carried_from: it.carried_from.clone(),
        deadline: it.deadline.clone(),
        note: it.note.clone(),
        overdue,
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

/// Markdown after a write plus a phrase warning for the caller to surface.
pub type Written = (String, Option<&'static str>);

/// Append `- [ ] text` to the day (section created in date order). A trailing
/// `due <when>` / `by <when>` phrase (or a legacy `⏰ YYYY-MM-DD`) becomes the
/// deadline; one that looks like a date but does not parse is kept as typed
/// and reported.
pub fn add_item(md: &str, date: &str, today: &str, text: &str) -> Result<Written, String> {
    let tk = split_tokens(text.lines().next().unwrap_or(""), today_date(today));
    if tk.text.is_empty() {
        return Err("empty to-do".into());
    }
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

pub fn toggle_item(md: &str, date: &str, today: &str, id: Option<&str>, text: Option<&str>, done: bool) -> Result<String, String> {
    with_item(md, date, today, id, text, |it| it.mark = if done { 'x' } else { ' ' })
}

/// Replace the text; a phrase in it sets the deadline, none keeps the old one.
pub fn edit_item(md: &str, date: &str, today: &str, id: &str, text: &str) -> Result<Written, String> {
    let tk = split_tokens(text.lines().next().unwrap_or(""), today_date(today));
    if tk.text.is_empty() {
        return Err("empty to-do".into());
    }
    let md = with_item(md, date, today, Some(id), None, |it| {
        it.text = tk.text;
        if tk.deadline.is_some() {
            it.deadline = tk.deadline;
        }
    })?;
    Ok((md, tk.warning))
}

pub fn set_deadline(md: &str, date: &str, today: &str, id: &str, deadline: Option<&str>) -> Result<String, String> {
    if let Some(d) = deadline
        && !is_date(d)
    {
        return Err(format!("deadline must be YYYY-MM-DD, got {d:?}"));
    }
    with_item(md, date, today, Some(id), None, |it| it.deadline = deadline.map(str::to_string))
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

fn local_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

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
    date: String,
    f: impl FnOnce(&str, &str) -> Result<Written, String> + Send + 'static,
) -> Json<Value> {
    if let Err(m) = check_date(&date) {
        return Json(json!({"error": m}));
    }
    let human = st.human;
    let hot = st.hot.clone();
    let today = local_today();
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
        let (new_md, warning) = match f(&md, &today) {
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
}

async fn get_day(State(st): State<ApiState>, Query(q): Query<DayQuery>) -> Json<Value> {
    let today = local_today();
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
}

async fn add(State(st): State<ApiState>, Json(req): Json<AddReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| add_item(md, &date, today, &req.text)).await
}

#[derive(Deserialize)]
struct ParseQuery {
    #[serde(default)]
    text: String,
}

/// The UI's live hint while typing: what the daemon would make of `text`.
async fn parse(Query(q): Query<ParseQuery>) -> Json<Value> {
    let tk = split_tokens(q.text.lines().next().unwrap_or(""), today_date(&local_today()));
    let mut v = json!({"text": tk.text, "deadline": tk.deadline});
    if let Some(w) = tk.warning {
        v["warning"] = json!(w);
    }
    Json(v)
}

#[derive(Deserialize)]
struct ToggleReq {
    date: String,
    item_id: Option<String>,
    text: Option<String>,
    done: bool,
}

async fn toggle(State(st): State<ApiState>, Json(req): Json<ToggleReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| {
        toggle_item(md, &date, today, req.item_id.as_deref(), req.text.as_deref(), req.done).map(|m| (m, None))
    })
    .await
}

#[derive(Deserialize)]
struct EditReq {
    date: String,
    item_id: String,
    text: String,
}

async fn edit(State(st): State<ApiState>, Json(req): Json<EditReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| edit_item(md, &date, today, &req.item_id, &req.text)).await
}

#[derive(Deserialize)]
struct RemoveReq {
    date: String,
    item_id: String,
}

async fn remove(State(st): State<ApiState>, Json(req): Json<RemoveReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| remove_item(md, &date, today, &req.item_id).map(|m| (m, None))).await
}

#[derive(Deserialize)]
struct MoveReq {
    date: String,
    item_id: String,
    to_date: String,
}

async fn mv(State(st): State<ApiState>, Json(req): Json<MoveReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| move_item(md, &date, today, &req.item_id, &req.to_date).map(|m| (m, None))).await
}

#[derive(Deserialize)]
struct DeadlineReq {
    date: String,
    item_id: String,
    /// null / absent clears
    #[serde(default)]
    deadline: Option<String>,
}

async fn deadline(State(st): State<ApiState>, Json(req): Json<DeadlineReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| {
        let dl = req.deadline.as_deref().map(str::trim).filter(|d| !d.is_empty());
        set_deadline(md, &date, today, &req.item_id, dl).map(|m| (m, None))
    })
    .await
}

#[derive(Deserialize)]
struct NoteReq {
    date: String,
    item_id: String,
    #[serde(default)]
    note: String,
}

async fn note(State(st): State<ApiState>, Json(req): Json<NoteReq>) -> Json<Value> {
    let date = req.date.clone();
    mutate(st, req.date, move |md, today| set_note(md, &date, today, &req.item_id, &req.note).map(|m| (m, None))).await
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/todo", get(get_day).post(add))
        .route("/api/todo/parse", get(parse))
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

    fn add(md: &str, date: &str, text: &str) -> Result<String, String> {
        add_item(md, date, T, text).map(|(m, _)| m)
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
        assert!(!items[1].overdue && !items[1].due_soon, "done items never nag");
        assert_eq!(items[2].carried_from.as_deref(), Some("2026-09-08"));
        assert_eq!(items[2].deadline.as_deref(), Some("2026-09-08"));
        assert!(items[2].overdue && !items[2].due_soon);
        let today = parse_day(DOC, T, T);
        assert!(today[0].due_soon && !today[0].overdue);
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
        let (md, warn) = add_item(&md, T, T, "rent due 31/2").unwrap();
        assert_eq!(warn, Some(crate::due::WARNING));
        assert!(md.ends_with("- [ ] rent due 31/2\n"), "{md}");
        // a mid-sentence due is text
        let (md, warn) = add_item(&md, T, T, "read the due diligence doc").unwrap();
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
        let (md, _) = edit_item(&md, "2026-09-09", T, &items[2].id, "rotate ALL tokens").unwrap();
        assert!(md.contains("- [ ] rotate ALL tokens · due 2026-09-08 (carried from 2026-09-08)\n"));
        // an edit with a phrase moves the deadline
        let items = parse_day(&md, "2026-09-09", T);
        let (md, warn) = edit_item(&md, "2026-09-09", T, &items[2].id, "rotate ALL tokens by sat").unwrap();
        assert!(warn.is_none());
        assert!(md.contains("- [ ] rotate ALL tokens · due 2026-09-12 (carried from 2026-09-08)\n"), "{md}");
        // an edit with an unreadable phrase keeps the text as typed and the old deadline
        let (md, warn) = edit_item(&md, "2026-09-09", T, &items[2].id, "rotate ALL tokens by 32/9").unwrap();
        assert_eq!(warn, Some(crate::due::WARNING));
        assert!(md.contains("- [ ] rotate ALL tokens by 32/9 · due 2026-09-12 (carried from 2026-09-08)\n"), "{md}");
        let (md, _) = edit_item(&md, "2026-09-09", T, &parse_day(&md, "2026-09-09", T)[2].id, "rotate ALL tokens").unwrap();
        let items = parse_day(&md, "2026-09-09", T);
        // deadline set / clear
        let md = set_deadline(&md, "2026-09-09", T, &items[0].id, Some("2026-09-30")).unwrap();
        assert!(md.contains("- [x] call the bank · due 2026-09-30\n"));
        let md = set_deadline(&md, "2026-09-09", T, &items[2].id, None).unwrap();
        assert!(md.contains("- [ ] rotate ALL tokens (carried from 2026-09-08)\n"));
        assert!(set_deadline(&md, "2026-09-09", T, &items[0].id, Some("soon")).is_err());
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
        // the original origin and the (overdue) deadline are kept
        assert_eq!(today[2].carried_from.as_deref(), Some("2026-09-08"));
        assert_eq!(today[2].deadline.as_deref(), Some("2026-09-08"));
        assert!(today[2].overdue);
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
        // today: carry-forward pulls the open one, once, deadline intact (now overdue)
        let today = local_today();
        let g = call(&app, "GET", &format!("/api/todo?date={today}"), None).await;
        assert_eq!(g["carried"], 1, "{g}");
        assert_eq!(g["items"][0]["text"], "call the bank");
        assert_eq!(g["items"][0]["carried_from"], "2026-09-09");
        assert_eq!(g["items"][0]["overdue"], true);
        assert_eq!(g["prev_date"], "2026-09-09");
        assert_eq!(g["today"], today);
        let g2 = call(&app, "GET", &format!("/api/todo?date={today}"), None).await;
        assert_eq!(g2["carried"], 0);
        assert_eq!(g2["items"].as_array().unwrap().len(), 1);
        let y = call(&app, "GET", "/api/todo?date=2026-09-09", None).await;
        assert_eq!(y["items"][0]["carried"], true, "{y}");
        assert_eq!(y["carried"], 0);
        // note, deadline, edit, move, remove by id
        let tid = g2["items"][0]["id"].as_str().unwrap().to_string();
        let n = call(&app, "POST", "/api/todo/note", Some(json!({"date": today, "item_id": tid, "note": "about the mortgage"}))).await;
        assert_eq!(n["items"][0]["note"], "about the mortgage", "{n}");
        let d = call(&app, "POST", "/api/todo/deadline", Some(json!({"date": today, "item_id": tid, "deadline": null}))).await;
        assert!(d["items"][0].get("deadline").is_none(), "{d}");
        let e = call(&app, "POST", "/api/todo/edit", Some(json!({"date": today, "item_id": tid, "text": "call the bank today"}))).await;
        assert_eq!(e["items"][0]["text"], "call the bank today");
        assert_eq!(e["items"][0]["note"], "about the mortgage");
        // an edit with a phrase sets the deadline relative to the daemon's today
        let tid = e["items"][0]["id"].as_str().unwrap().to_string();
        let e = call(&app, "POST", "/api/todo/edit", Some(json!({"date": today, "item_id": tid, "text": "call the bank today due tomorrow"}))).await;
        assert_eq!(e["items"][0]["text"], "call the bank today", "{e}");
        let tomorrow = (chrono::Local::now().date_naive() + chrono::Duration::days(1)).to_string();
        assert_eq!(e["items"][0]["deadline"], tomorrow);
        assert_eq!(e["items"][0]["due_soon"], true);
        let tid = e["items"][0]["id"].as_str().unwrap().to_string();
        let m = call(&app, "POST", "/api/todo/move", Some(json!({"date": today, "item_id": tid, "to_date": "2099-01-01"}))).await;
        assert_eq!(m["items"].as_array().unwrap().len(), 0, "answers with the source day: {m}");
        let far = call(&app, "GET", "/api/todo?date=2099-01-01", None).await;
        assert_eq!(far["items"][0]["text"], "call the bank today");
        let tid = far["items"][0]["id"].as_str().unwrap().to_string();
        let r = call(&app, "POST", "/api/todo/remove", Some(json!({"date": "2099-01-01", "item_id": tid}))).await;
        assert_eq!(r["items"].as_array().unwrap().len(), 0);
        // an unreadable phrase: kept as typed, warned about
        let w = call(&app, "POST", "/api/todo", Some(json!({"date": today, "text": "rent due 31/2"}))).await;
        assert_eq!(w["warning"], crate::due::WARNING, "{w}");
        let last = w["items"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["text"], "rent due 31/2");
        assert!(last.get("deadline").is_none());
        // the live hint
        let p = call(&app, "GET", "/api/todo/parse?text=pay%20rent%20by%20tomorrow", None).await;
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
        let e = call(&app, "POST", "/api/todo", Some(json!({"date": today, "text": "  "}))).await;
        assert!(e["error"].is_string());
    }
}
