//! Colour covers for the EPUB library. Books exported from a Kindle often
//! carry a black-and-white cover; both e-readers are e-ink, so colour only
//! matters where Taisce shows a cover (the web app, the Apple app, OPDS
//! thumbnails). Taisce never rewrites a book's bytes: a colour cover is
//! stored BESIDE the book (`library/<user>/<book id>.cover-color.<ext>`) and
//! every cover route serves it while it is the book's chosen cover
//! (`Book::shown_cover_key`).
//!
//! - Greyscale detection: [`analyse`] decodes the cover (bounded), shrinks
//!   it, and measures Hasler and Süsstrunk's colourfulness; below
//!   [`GREY_MAX`] the cover is greyscale. Ingest stores the verdict; the
//!   cover review fills it in for books shelved before it existed.
//! - Candidates come from Open Library only: the cover of each ISBN among
//!   the OPF's identifiers, then title and author searches
//!   ([`search_variants`]: as is, without series brackets, without a
//!   subtitle, then the surname alone). The server
//!   builds every URL itself, fetches only from Open Library's two hosts
//!   (and the archive.org hosts their redirects lead to), validates each
//!   image ([`validate_candidate`]) and caches the survivors in the store.
//!   Clients see a candidate as an id the server minted, never a URL.
//! - The person chooses (`POST /api/books/{id}/cover`): a candidate, or the
//!   book's own cover. A library pass accepts with `unless_chosen`, so it
//!   never overwrites a cover the person chose.
//!
//! Routes (a person's own app or web session: the library router refuses
//! connector writes, and none of this is an MCP tool):
//!
//! - `GET /api/books/{id}/cover-candidates` → the cached candidates
//! - `POST /api/books/{id}/cover-candidates` `{refresh?, title?, author?}`
//!   → asks Open Library (unless a lookup from the last 30 days is cached;
//!   a cached "none found" from an older [`MATCHER_VERSION`] is not) and
//!   caches. `title` (+ `author`) searches with the person's own terms.
//! - `GET /api/books/{id}/cover-candidates/{candidate}` → the image
//! - `POST /api/books/{id}/cover` `{candidate} | {use: "embedded"}`,
//!   `unless_chosen?` → the book
//! - `GET /api/books/cover-review` → greyscale books, then books with no
//!   cover at all, each with its best cached candidate, and how many
//!   covers are not looked at yet
//! - `POST /api/books/cover-review` → looks at the next batch of covers
//!   shelved before greyscale detection, then answers like the GET

use crate::library::{LibraryState, book_json, err, fail, not_found};
use crate::store_ext::{blocking, with_store};
use crate::viewer::Viewer;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

use taisce_store::books::{Book, CoverCandidate, CoverChoice, CoverPick, NewCoverCandidate};
use taisce_store::StoreError;
use url::Url;
use uuid::Uuid;

/// Colourfulness below this is a greyscale cover. A true greyscale JPEG
/// measures 0–2 (chroma noise), a faint cream or warm tint a few points;
/// real colour covers measured 19 (Open Library's muted Horus Rising) and
/// 32 (Foundation), and both measured 0.0 converted to grey.
pub const GREY_MAX: f64 = 10.0;
/// A candidate must measure at least this to be offered as a COLOUR cover.
pub const COLOUR_MIN: f64 = 15.0;
/// Open Library's -L covers are 500 px tall; anything under this is a
/// thumbnail that would look worse than the greyscale original.
const MIN_HEIGHT: u32 = 300;
const MIN_WIDTH: u32 = 150;
/// A cover is taller than wide; a candidate wider than tall, or more than
/// twice as tall as wide, is a scan of something else.
const MIN_ASPECT: f64 = 1.0;
const MAX_ASPECT: f64 = 2.0;
/// Bytes read of one Open Library answer (covers are ~40–150 KB).
const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;
/// Candidates offered per book.
pub const MAX_CANDIDATES: usize = 4;
/// Cover images fetched per lookup at most (some fail validation).
const MAX_COVER_FETCHES: usize = 8;
/// Title searches per lookup at most ([`search_variants`]).
const MAX_SEARCHES: usize = 4;
/// The matching rules' version, stamped on every lookup. A cached "none
/// found" from an older version is stale (newer rules may find what the
/// old ones missed): the next find asks again. Bump it whenever the
/// searches or the matching change.
/// - 1: one search with the normalised title (unstamped rows)
/// - 2: [`search_variants`] in turn; series-prefixed titles; the whole
///   title (subtitle kept) matches too; padded titles; authors written in
///   another script
pub const MATCHER_VERSION: i64 = 2;
/// ISBNs tried per lookup.
const MAX_ISBNS: usize = 2;
/// A lookup is cached this long; a person's "Find again" refreshes it.
const LOOKUP_TTL: i64 = 30 * 86_400;
/// Covers the review looks at per POST (each is one S3 GET and a decode).
const SCAN_BATCH: usize = 20;
/// Cap on a stored cover the review reads back (ingest's cap).
const MAX_STORED_COVER_BYTES: usize = 16 * 1024 * 1024;

// ---- judging an image ----

/// What [`analyse`] measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Analysis {
    pub width: u32,
    pub height: u32,
    /// Hasler and Süsstrunk's M: 0 for grey, ~15 slightly colourful, ~33
    /// moderately, 45+ highly.
    pub colorfulness: f64,
}

/// Decode an image (JPEG, PNG, WebP or GIF; bounded: at most 10 000 px a
/// side and 128 MB of decode buffers) and measure it on a 128 px thumbnail.
/// None when it does not decode — an SVG cover, a corrupt file.
pub fn analyse(bytes: &[u8]) -> Option<Analysis> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10_000);
    limits.max_image_height = Some(10_000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (width, height) = (img.width(), img.height());
    let small = img.thumbnail(128, 128).to_rgb8();
    Some(Analysis { width, height, colorfulness: colorfulness(&small) })
}

/// Hasler and Süsstrunk (2003): over the opponent channels rg = R − G and
/// yb = (R + G)/2 − B, M = σ + 0.3·μ of their joint spread and mean.
fn colorfulness(img: &image::RgbImage) -> f64 {
    let n = (img.width() as f64) * (img.height() as f64);
    if n == 0.0 {
        return 0.0;
    }
    let (mut srg, mut syb, mut srg2, mut syb2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for p in img.pixels() {
        let [r, g, b] = p.0.map(f64::from);
        let rg = r - g;
        let yb = 0.5 * (r + g) - b;
        srg += rg;
        syb += yb;
        srg2 += rg * rg;
        syb2 += yb * yb;
    }
    let (mrg, myb) = (srg / n, syb / n);
    let vrg = (srg2 / n - mrg * mrg).max(0.0);
    let vyb = (syb2 / n - myb * myb).max(0.0);
    (vrg + vyb).sqrt() + 0.3 * (mrg * mrg + myb * myb).sqrt()
}

pub fn is_greyscale(a: &Analysis) -> bool {
    a.colorfulness < GREY_MAX
}

/// The verdict ingest and the review store for a cover's bytes: greyscale
/// or not. A cover that will not decode (an SVG) is judged "not
/// greyscale": the pass never offers to replace what it cannot see, and
/// the book's details still can.
pub fn cover_verdict(bytes: &[u8]) -> bool {
    analyse(bytes).is_some_and(|a| is_greyscale(&a))
}

/// The extension of an image's actual format (never the server's word).
fn image_ext(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        image::ImageFormat::Jpeg => Some("jpg"),
        image::ImageFormat::Png => Some("png"),
        image::ImageFormat::WebP => Some("webp"),
        image::ImageFormat::Gif => Some("gif"),
        _ => None,
    }
}

/// An Open Library image worth offering: it decodes, it is a cover-shaped
/// image at least 300 px tall (never the 1×1 "no cover" placeholder), and
/// it is actually colourful. The reason when not.
pub fn validate_candidate(bytes: &[u8]) -> Result<(Analysis, &'static str), String> {
    if bytes.len() > MAX_FETCH_BYTES {
        return Err("over 4 MB".into());
    }
    let ext = image_ext(bytes).ok_or("not a JPEG, PNG, WebP or GIF")?;
    let a = analyse(bytes).ok_or("does not decode")?;
    if a.height < MIN_HEIGHT || a.width < MIN_WIDTH {
        return Err(format!("{}×{} is too small", a.width, a.height));
    }
    let aspect = a.height as f64 / a.width as f64;
    if !(MIN_ASPECT..=MAX_ASPECT).contains(&aspect) {
        return Err(format!("{}×{} is not cover-shaped", a.width, a.height));
    }
    if a.colorfulness < COLOUR_MIN {
        return Err(format!("not colourful ({:.1})", a.colorfulness));
    }
    Ok((a, ext))
}

// ---- ISBNs from the OPF ----

/// The ISBNs among an OPF's `dc:identifier`s (`(scheme attribute, text)`),
/// as ISBN-13, in order, deduplicated. An identifier counts when it says it
/// is one — `opf:scheme="ISBN"`, a `urn:isbn:` / `isbn:` / `ISBN ` prefix —
/// or is a bare 13-digit 978/979 number; either way its check digit must
/// hold. ISBN-10s become their ISBN-13 (Open Library knows both).
pub fn isbns_from_identifiers(ids: &[(Option<String>, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (scheme, text) in ids {
        let said = scheme.as_deref().is_some_and(|s| s.trim().eq_ignore_ascii_case("isbn"));
        let t = text.trim();
        let lower = t.to_ascii_lowercase();
        let (prefixed, rest) = ["urn:isbn:", "isbn:", "isbn-13:", "isbn-10:", "isbn13:", "isbn10:", "isbn "]
            .iter()
            .find_map(|p| lower.strip_prefix(p).map(|r| (true, r.to_string())))
            .unwrap_or((false, lower.clone()));
        let digits: String = rest.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
        let isbn = match digits.len() {
            13 if isbn13_valid(&digits) => Some(digits),
            10 if (said || prefixed) && isbn10_valid(&digits) => Some(isbn10_to_13(&digits)),
            _ => None,
        };
        let Some(isbn) = isbn else { continue };
        // a bare number counts only as an ISBN-13 (the 978/979 prefix and
        // its check digit make a stray match vanishingly unlikely)
        if (said || prefixed || isbn.starts_with("978") || isbn.starts_with("979")) && !out.contains(&isbn) {
            out.push(isbn);
        }
    }
    out
}

fn isbn13_valid(d: &str) -> bool {
    if d.len() != 13 || !d.bytes().all(|b| b.is_ascii_digit()) || !(d.starts_with("978") || d.starts_with("979")) {
        return false;
    }
    let sum: u32 = d.bytes().enumerate().map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 }).sum();
    sum % 10 == 0
}

fn isbn10_valid(d: &str) -> bool {
    let b = d.as_bytes();
    if b.len() != 10 || !b[..9].iter().all(u8::is_ascii_digit) || !(b[9].is_ascii_digit() || b[9] == b'x') {
        return false;
    }
    let sum: u32 = b
        .iter()
        .enumerate()
        .map(|(i, &c)| (10 - i as u32) * if c == b'x' { 10 } else { u32::from(c - b'0') })
        .sum();
    sum % 11 == 0
}

fn isbn10_to_13(d: &str) -> String {
    let core = format!("978{}", &d[..9]);
    let sum: u32 = core.bytes().enumerate().map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 }).sum();
    format!("{core}{}", (10 - sum % 10) % 10)
}

// ---- matching search results to the book ----

/// A title for matching: lowercased letters and digits, a series tag in
/// brackets dropped ("Horus Rising (The Horus Heresy Book 1)"), a
/// subtitle after ':' or a dash dropped (" - " too) unless `whole`, a
/// leading article dropped, and anything after a run of three or more
/// spaces dropped (Open Library pads some titles before a series blurb:
/// "The First Heretic                Warhammer 40000 Novels").
fn norm_words(t: &str, whole: bool) -> String {
    let t = padded_head(t);
    let t = if whole { t } else { t.split(" - ").next().unwrap_or(t) };
    let mut s = String::new();
    let mut depth = 0usize;
    for c in t.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ':' | '–' | '—' if depth == 0 && !whole => break,
            c if depth == 0 => s.push(c),
            _ => {}
        }
    }
    let words = words(&s);
    let words = match words.first().map(String::as_str) {
        Some("the" | "a" | "an") if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    words.join(" ")
}

fn norm_title(t: &str) -> String {
    norm_words(t, false)
}

/// The text before a run of three or more whitespace characters.
fn padded_head(t: &str) -> &str {
    let t = t.trim();
    let mut run = 0usize;
    for (i, c) in t.char_indices() {
        if c.is_whitespace() {
            run += 1;
            if run == 3 {
                let head = t[..i].trim_end();
                return if words(head).is_empty() { t } else { head };
            }
        } else {
            run = 0;
        }
    }
    t
}

/// Words of a title fragment: lowercased letters and digits, nothing else.
fn words(t: &str) -> Vec<String> {
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// A title without its trailing parenthetical or bracketed groups, case
/// kept: "Saturnine (The Horus Heresy: Siege of Terra Book 4)" →
/// "Saturnine". The title itself when nothing would remain.
fn strip_groups(t: &str) -> String {
    let mut s = t.trim();
    while let Some(close) = s.chars().last().filter(|c| matches!(c, ')' | ']')) {
        let open = if close == ')' { '(' } else { '[' };
        let mut depth = 0usize;
        let mut cut = None;
        for (i, c) in s.char_indices().rev() {
            if c == close {
                depth += 1;
            } else if c == open {
                depth -= 1;
                if depth == 0 {
                    cut = Some(i);
                    break;
                }
            }
        }
        match cut {
            Some(i) if !words(&s[..i]).is_empty() => s = s[..i].trim_end(),
            _ => break,
        }
    }
    s.to_string()
}

/// The text inside a title's trailing groups, as series words: the
/// leading article and a trailing "Book 4" / "#4" / "Vol. 2" dropped
/// ("Helsreach (Space Marine Battles)" → "space marine battles").
fn series_words(t: &str) -> Vec<String> {
    let t = t.trim();
    let kept = strip_groups(t);
    if kept.len() >= t.len() {
        return Vec::new();
    }
    let mut w = words(&t[kept.len()..]);
    while w.last().is_some_and(|x| x.chars().all(|c| c.is_ascii_digit()) || NUMBERING.contains(&x.as_str())) {
        w.pop();
    }
    if matches!(w.first().map(String::as_str), Some("the" | "a" | "an")) && w.len() > 1 {
        w.remove(0);
    }
    w
}

/// Words that number a book in a series ("Book 4", "nº 01/04", "Vol. 2").
const NUMBERING: [&str; 11] = ["book", "books", "vol", "volume", "part", "no", "nº", "n", "number", "libro", "tomo"];

/// The searches to try, in order, until one finds a matching cover: the
/// title as it is; without its trailing series groups; without a
/// subtitle after ':' (when a word remains); then the stripped title with
/// the author's surname alone. Each keeps the author.
pub fn search_variants(title: &str, author: &str) -> Vec<(String, String)> {
    let full = title.trim().to_string();
    let stripped = strip_groups(&full);
    let short = stripped.split_once(':').map(|(head, _)| head.trim().to_string()).filter(|h| !words(h).is_empty());
    let author = author.trim().to_string();
    let mut out: Vec<(String, String)> = Vec::new();
    let push = |t: String, a: String, out: &mut Vec<(String, String)>| {
        if !t.is_empty() && !out.iter().any(|(ot, oa)| ot.eq_ignore_ascii_case(&t) && oa.eq_ignore_ascii_case(&a)) {
            out.push((t, a));
        }
    };
    push(full, author.clone(), &mut out);
    push(stripped.clone(), author.clone(), &mut out);
    if let Some(short) = short {
        push(short, author.clone(), &mut out);
    }
    if let Some(surname) = surname(&author).filter(|s| !s.eq_ignore_ascii_case(&author)) {
        push(stripped, surname, &mut out);
    }
    out
}

/// The first author's surname: "Dan Abnett" and "Abnett, Dan" → "Abnett",
/// "Aaron Dembski-Bowden & Graham McNeill" → "Dembski-Bowden".
fn surname(author: &str) -> Option<String> {
    let first = author.split([';', '&']).next()?.split(" and ").next()?.trim();
    let s = match first.split_once(',') {
        Some((last, _)) => last.trim(),
        None => first.split_whitespace().last()?,
    };
    (!words(s).is_empty()).then(|| s.to_string())
}

/// How a search result's title matches the book's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleMatch {
    /// the same title once both are normalised (series groups, subtitle,
    /// a leading article dropped)
    Exact,
    /// Open Library's title is the book's series, a number and the title
    /// ("Space Marine Battles nº 01/04 Helsreach" for "Helsreach (Space
    /// Marine Battles)"): ranked after exact matches
    SeriesPrefixed,
}

fn title_match(book: &str, found: &str) -> Option<TitleMatch> {
    // exact, after the normalising: a prefix would let "Foundation" take
    // "Foundation and Empire"'s cover
    let (a, b) = (norm_title(book), norm_title(found));
    if a.is_empty() {
        return None;
    }
    // the whole title too, subtitle and all: "2001: A Space Odyssey" is
    // Open Library's "2001 A SPACE ODYSSEY"
    if a == b || norm_words(book, true) == norm_words(found, true) {
        return Some(TitleMatch::Exact);
    }
    // the series prefix: only the book's OWN series (from its brackets),
    // then only numbering, then exactly the title
    let series = series_words(book);
    if series.is_empty() {
        return None;
    }
    let fw = words(&strip_groups(found));
    let rest = fw.strip_prefix(series.as_slice())?;
    let skip = rest.iter().take_while(|w| w.chars().all(|c| c.is_ascii_digit()) || NUMBERING.contains(&w.as_str())).count();
    (rest[skip..].join(" ") == a).then_some(TitleMatch::SeriesPrefixed)
}

#[cfg(test)]
fn title_matches(book: &str, found: &str) -> bool {
    title_match(book, found) == Some(TitleMatch::Exact)
}

/// The surname-ish last word of each name ("Abnett, Dan" and "Dan Abnett"
/// both give "abnett" among their words).
fn name_words(n: &str) -> HashSet<String> {
    n.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 1).map(str::to_lowercase).collect()
}

#[cfg(test)]
fn author_matches(book: &str, found: &[String]) -> bool {
    author_match(book, found).is_some()
}

/// Whether a result's authors are the book's: `Some(true)` a shared name
/// word (or no author of ours to check); `Some(false)` when the only names
/// that could match are in another script ("刘慈欣" for Cixin Liu, "村上春樹"
/// for Haruki Murakami: Open Library's search matched them through its
/// alternate names, ours cannot compare), ranked after real matches;
/// None when they are someone else.
fn author_match(book: &str, found: &[String]) -> Option<bool> {
    let want = name_words(book);
    if want.is_empty() || found.iter().any(|f| !name_words(f).is_disjoint(&want)) {
        return Some(true);
    }
    found.iter().any(|f| other_script(f)).then_some(false)
}

/// A name with letters but none of them Latin.
fn other_script(n: &str) -> bool {
    let latin = |c: char| c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c);
    n.chars().any(char::is_alphabetic) && !n.chars().any(|c| c.is_alphabetic() && latin(c))
}

// ---- Open Library ----

/// Hosts a request may START at: Open Library's API and its covers.
const OL_HOSTS: [&str; 2] = ["openlibrary.org", "covers.openlibrary.org"];

/// Hosts a redirect may lead to: Open Library's own, and the Internet
/// Archive's, where covers.openlibrary.org sends every image (archive.org,
/// then a numbered `ia…us.archive.org` node).
fn redirect_allowed(u: &Url) -> bool {
    u.scheme() == "https"
        && u.host_str().is_some_and(|h| {
            OL_HOSTS.contains(&h) || h == "archive.org" || (h.ends_with(".us.archive.org") && h.starts_with("ia"))
        })
}

fn start_allowed(u: &Url) -> bool {
    u.scheme() == "https" && u.host_str().is_some_and(|h| OL_HOSTS.contains(&h)) && u.port().is_none()
}

/// Tests' stand-in Open Library: canned answers by exact URL (anything
/// else is a 404, like the covers API with `default=false`), and the log of
/// every URL asked for.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MockOpenLibrary {
    pub answers: std::sync::Mutex<std::collections::HashMap<String, (u16, Vec<u8>)>>,
    pub asked: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl MockOpenLibrary {
    pub fn answer(&self, url: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.answers.lock().unwrap().insert(url.to_string(), (status, body.into()));
    }
}

/// The Open Library client: one per daemon, at most two requests in flight
/// across every lookup, a descriptive User-Agent with a contact.
pub struct OpenLibrary {
    http: reqwest::Client,
    gate: tokio::sync::Semaphore,
    #[cfg(test)]
    pub(crate) mock: std::sync::Arc<MockOpenLibrary>,
}

/// What one lookup found.
pub struct Lookup {
    pub candidates: Vec<NewCoverCandidate>,
    /// Open Library failed (unreachable, 5xx, a timeout) somewhere along
    /// the way, so "nothing found" may be wrong: not cached.
    pub unavailable: bool,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    docs: Vec<SearchDoc>,
}

#[derive(Deserialize)]
struct SearchDoc {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author_name: Vec<String>,
    #[serde(default)]
    cover_i: Option<i64>,
}

impl OpenLibrary {
    /// `contact`: a URL the User-Agent names (the server's public URL),
    /// as Open Library asks of API clients.
    pub fn new(contact: &str) -> anyhow::Result<OpenLibrary> {
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .redirect(policy)
            .user_agent(format!("Taisce/{} (+{contact}; colour covers)", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(OpenLibrary {
            http,
            gate: tokio::sync::Semaphore::new(2),
            #[cfg(test)]
            mock: Default::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> OpenLibrary {
        let _ = rustls::crypto::ring::default_provider().install_default();
        OpenLibrary::new("https://taisce.test").unwrap()
    }

    /// GET one URL the server built. `Ok(None)` = 404 (Open Library's "no
    /// such cover"); `Err` = it failed, which a lookup reports as
    /// unavailable. Never a URL from a client: [`start_allowed`] refuses
    /// anything off Open Library's hosts before a byte is sent.
    async fn get(&self, url: Url) -> Result<Option<Vec<u8>>, String> {
        if !start_allowed(&url) {
            return Err(format!("refused to fetch {url}: not Open Library"));
        }
        let _permit = self.gate.acquire().await.map_err(|e| e.to_string())?;
        #[cfg(test)]
        {
            self.mock.asked.lock().unwrap().push(url.to_string());
            return match self.mock.answers.lock().unwrap().get(url.as_str()).cloned() {
                None | Some((404, _)) => Ok(None),
                Some((200, body)) => Ok(Some(body)),
                Some((status, _)) => Err(format!("Open Library answered {status}")),
            };
        }
        #[allow(unreachable_code)]
        {
            let mut res = self.http.get(url).send().await.map_err(|e| format!("Open Library: {e}"))?;
            if res.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !res.status().is_success() {
                return Err(format!("Open Library answered {}", res.status()));
            }
            let mut out = Vec::new();
            while let Some(chunk) = res.chunk().await.map_err(|e| format!("Open Library: {e}"))? {
                if out.len() + chunk.len() > MAX_FETCH_BYTES {
                    return Err("Open Library sent more than 4 MB".into());
                }
                out.extend_from_slice(&chunk);
            }
            Ok(Some(out))
        }
    }

    /// `https://covers.openlibrary.org/b/isbn/<isbn>-L.jpg?default=false`
    /// (404 when Open Library has none, not its placeholder).
    pub fn isbn_cover_url(isbn: &str) -> Url {
        let mut u = Url::parse("https://covers.openlibrary.org/b/isbn/").unwrap();
        u.path_segments_mut().unwrap().pop_if_empty().push(&format!("{isbn}-L.jpg"));
        u.set_query(Some("default=false"));
        u
    }

    /// `https://covers.openlibrary.org/b/id/<cover id>-L.jpg?default=false`
    pub fn id_cover_url(cover_i: i64) -> Url {
        let mut u = Url::parse("https://covers.openlibrary.org/b/id/").unwrap();
        u.path_segments_mut().unwrap().pop_if_empty().push(&format!("{cover_i}-L.jpg"));
        u.set_query(Some("default=false"));
        u
    }

    /// One search: title and author as given ([`search_variants`] makes
    /// them), five results, only the fields used.
    pub fn search_url(title: &str, author: &str) -> Url {
        let mut u = Url::parse("https://openlibrary.org/search.json").unwrap();
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("title", title.trim());
            if !author.trim().is_empty() {
                q.append_pair("author", author.trim());
            }
            q.append_pair("limit", "5");
            q.append_pair("fields", "key,title,author_name,cover_i,isbn,first_publish_year");
        }
        u
    }

    /// Up to [`MAX_CANDIDATES`] validated colour covers for a book, best
    /// first: each ISBN's own cover (that edition), then search results
    /// whose title and author match, in Open Library's order (exact title
    /// matches before series-prefixed ones). The searches are
    /// [`search_variants`], tried in order until one yields an exactly
    /// matching cover. Duplicates (the same bytes by two routes) are
    /// offered once. Requests run one at a time per lookup and two across
    /// the daemon.
    pub async fn lookup(&self, title: &str, author: &str, isbns: &[String]) -> Lookup {
        let mut out: Vec<NewCoverCandidate> = Vec::new();
        let mut weak: Vec<NewCoverCandidate> = Vec::new();
        let mut seen: HashSet<[u8; 32]> = HashSet::new();
        let mut unavailable = false;
        let mut fetches = 0usize;
        let mut offer = |bytes: Vec<u8>, source: &str, source_ref: String, out: &mut Vec<NewCoverCandidate>| -> bool {
            use sha2::Digest as _;
            let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
            if !seen.insert(digest) {
                return false;
            }
            match validate_candidate(&bytes) {
                Ok((a, ext)) => {
                    out.push(NewCoverCandidate {
                        source: source.into(),
                        source_ref,
                        image: bytes,
                        ext: ext.into(),
                        width: a.width.into(),
                        height: a.height.into(),
                        colorfulness: a.colorfulness,
                    });
                    true
                }
                Err(why) => {
                    tracing::debug!(source, "colour covers: candidate refused: {why}");
                    false
                }
            }
        };
        for isbn in isbns.iter().take(MAX_ISBNS) {
            if out.len() >= MAX_CANDIDATES || fetches >= MAX_COVER_FETCHES {
                break;
            }
            fetches += 1;
            match self.get(Self::isbn_cover_url(isbn)).await {
                Ok(Some(bytes)) => {
                    offer(bytes, "isbn", isbn.clone(), &mut out);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::info!("colour covers: {e}");
                    unavailable = true;
                }
            }
        }
        let mut covers_seen: HashSet<i64> = HashSet::new();
        for (q_title, q_author) in search_variants(title, author).into_iter().take(MAX_SEARCHES) {
            if out.len() + weak.len() >= MAX_CANDIDATES || fetches >= MAX_COVER_FETCHES {
                break;
            }
            let docs = match self.get(Self::search_url(&q_title, &q_author)).await {
                Ok(Some(body)) => match serde_json::from_slice::<SearchResponse>(&body) {
                    Ok(r) => r.docs,
                    Err(e) => {
                        tracing::info!("colour covers: an unreadable search answer: {e}");
                        unavailable = true;
                        Vec::new()
                    }
                },
                Ok(None) => Vec::new(),
                Err(e) => {
                    tracing::info!("colour covers: {e}");
                    unavailable = true;
                    Vec::new()
                }
            };
            let mut found_exact = false;
            for d in docs {
                if out.len() + weak.len() >= MAX_CANDIDATES || fetches >= MAX_COVER_FETCHES {
                    break;
                }
                let Some(cover_i) = d.cover_i.filter(|c| *c > 0) else { continue };
                // matched against the book's own title and author, whatever
                // the query was
                let Some(how) = d.title.as_deref().and_then(|t| title_match(title, t)) else { continue };
                let Some(same_author) = author_match(author, &d.author_name) else { continue };
                if !covers_seen.insert(cover_i) {
                    continue;
                }
                let strong = how == TitleMatch::Exact && same_author;
                fetches += 1;
                match self.get(Self::id_cover_url(cover_i)).await {
                    Ok(Some(bytes)) => {
                        let into = if strong { &mut out } else { &mut weak };
                        if offer(bytes, "search", cover_i.to_string(), into) && strong {
                            found_exact = true;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::info!("colour covers: {e}");
                        unavailable = true;
                    }
                }
            }
            if found_exact {
                break;
            }
        }
        out.extend(weak);
        out.truncate(MAX_CANDIDATES);
        Lookup { candidates: out, unavailable }
    }
}

// ---- routes ----

fn candidate_json(book: Uuid, c: &CoverCandidate) -> Value {
    json!({
        "id": c.id,
        "source": c.source.as_str(),
        "width": c.width,
        "height": c.height,
        "colorfulness": (c.colorfulness * 10.0).round() / 10.0,
        // the server's own route: a client never learns Open Library's URL
        "url": format!("/api/books/{book}/cover-candidates/{}", c.id),
    })
}

fn candidates_json(b: &Book, list: &[CoverCandidate], unavailable: bool) -> Value {
    json!({
        "book": book_json(b),
        "looked_up_at": b.color_lookup_at.map(crate::kosync::rfc3339),
        "candidates": list.iter().map(|c| candidate_json(b.id, c)).collect::<Vec<_>>(),
        "unavailable": unavailable,
    })
}

/// The person's library, or a 404 (LOCAL mode, no bucket).
fn person(st: &LibraryState, v: &Viewer) -> Result<Uuid, Response> {
    match (&st.s3, v.user) {
        (Some(_), Some(u)) => Ok(u),
        _ => Err(not_found()),
    }
}

pub(crate) async fn candidates_get(State(st): State<LibraryState>, v: Viewer, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = person(&st, &v) {
        return r;
    }
    let got = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<_> {
        let b = s.book_get(id)?;
        let c = s.book_cover_candidates(id)?;
        Ok((b, c))
    })
    .await;
    match got {
        Ok((b, c)) => Json(candidates_json(&b, &c, false)).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct FindReq {
    /// ask Open Library again even when a recent lookup is cached
    #[serde(default)]
    refresh: bool,
    /// search with these terms instead of the book's own (the person's
    /// "Search with a different title…"): always a fresh lookup, no ISBNs,
    /// results matched against these terms. The server still builds every
    /// URL itself.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

/// Longest custom search term accepted.
const MAX_TERM_CHARS: usize = 200;

/// A lookup the cache can answer with: one was made, and either it found
/// candidates or the current matcher made it (an older matcher's "none
/// found" is stale).
pub fn lookup_current(b: &Book, has_candidates: bool) -> bool {
    b.color_lookup_at.is_some() && (has_candidates || b.color_lookup_matcher.unwrap_or(1) >= MATCHER_VERSION)
}

pub(crate) async fn candidates_find(State(st): State<LibraryState>, v: Viewer, Path(id): Path<Uuid>, body: axum::body::Bytes) -> Response {
    if let Err(r) = person(&st, &v) {
        return r;
    }
    // an empty body is `{}`: the UI's plain "find" sends nothing
    let req: FindReq = if body.iter().all(u8::is_ascii_whitespace) {
        FindReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("bad body: {e}")),
        }
    };
    let got = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<_> {
        let b = s.book_get(id)?;
        let c = s.book_cover_candidates(id)?;
        Ok((b, c))
    })
    .await;
    let (book, cached) = match got {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let now = crate::auth::now();
    let custom = match custom_terms(&req) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let fresh = lookup_current(&book, !cached.is_empty()) && book.color_lookup_at.is_some_and(|at| now - at < LOOKUP_TTL);
    if custom.is_none() && !req.refresh && fresh {
        return Json(candidates_json(&book, &cached, false)).into_response();
    }
    let found = match &custom {
        Some((title, author)) => st.covers.lookup(title, author, &[]).await,
        None => {
            let isbns = match &book.isbns {
                Some(_) => book.isbn_list(),
                None => read_isbns(&st, &v, &book).await,
            };
            st.covers.lookup(&book.title, &book.author, &isbns).await
        }
    };
    if found.candidates.is_empty() && (found.unavailable || custom.is_some()) {
        // quietly: what was cached stays, nothing is stamped, a later
        // find asks again. A different title that found nothing answers
        // nothing, and leaves the book's own lookup as it was.
        let shown: &[CoverCandidate] = if custom.is_some() { &[] } else { &cached };
        return Json(candidates_json(&book, shown, found.unavailable)).into_response();
    }
    let unavailable = found.unavailable;
    let stored = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<_> {
        let c = s.book_cover_candidates_replace(id, &found.candidates, now, MATCHER_VERSION)?;
        Ok((s.book_get(id)?, c))
    })
    .await;
    match stored {
        Ok((b, c)) => Json(candidates_json(&b, &c, unavailable)).into_response(),
        Err(e) => fail(e),
    }
}

/// The person's own search terms, trimmed (an empty author = none), or
/// None for the book's; a 400 for an empty or over-long title.
fn custom_terms(req: &FindReq) -> Result<Option<(String, String)>, Response> {
    let Some(title) = req.title.as_deref() else {
        if req.author.is_some() {
            return Err(err(StatusCode::BAD_REQUEST, "a custom search needs a title"));
        }
        return Ok(None);
    };
    let clean = |t: &str| t.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string();
    let (title, author) = (clean(title), clean(req.author.as_deref().unwrap_or("")));
    if words(&title).is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "a custom search needs a title"));
    }
    if title.chars().count() > MAX_TERM_CHARS || author.chars().count() > MAX_TERM_CHARS {
        return Err(err(StatusCode::BAD_REQUEST, format!("search terms are at most {MAX_TERM_CHARS} characters")));
    }
    Ok(Some((title, author)))
}

/// A book shelved before ISBNs were kept: read them out of its EPUB (S3 →
/// a spool file → the OPF; never unpacked, never whole in memory) and
/// remember them, '' when it has none. Any failure is "no ISBNs" this time
/// (and nothing remembered, so the next lookup tries again).
async fn read_isbns(st: &LibraryState, v: &Viewer, book: &Book) -> Vec<String> {
    let Some(s3) = st.s3.clone() else { return Vec::new() };
    if tokio::fs::create_dir_all(&st.spool_dir).await.is_err() {
        return Vec::new();
    }
    let spool = crate::library::Spool::path_in(&st.spool_dir);
    match s3.get_to_file(&book.file_s3_key, &spool.path, crate::library::MAX_FILE_BYTES).await {
        Ok(true) => {}
        Ok(false) => return Vec::new(),
        Err(e) => {
            tracing::info!(book = %book.id, "colour covers: reading the EPUB for its ISBNs: {e:#}");
            return Vec::new();
        }
    }
    let path = spool.path.clone();
    let Ok(isbns) = blocking(move || crate::library::read_isbns(&path)).await else {
        return Vec::new();
    };
    let (id, grey, joined) = (book.id, book.cover_grey, isbns.join(","));
    let _ = with_store(&st.store, v.scope, move |s| s.book_set_cover_facts(id, grey, Some(&joined))).await;
    isbns
}

pub(crate) async fn candidate_image(State(st): State<LibraryState>, v: Viewer, Path((id, cid)): Path<(Uuid, Uuid)>) -> Response {
    if let Err(r) = person(&st, &v) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.book_cover_candidate(id, cid)).await {
        Ok(c) => {
            let mut r = c.image.into_response();
            let h = r.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(crate::library::cover_content_type(&c.ext)));
            // a candidate id names one image forever
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400, immutable"));
            h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
            r
        }
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
pub(crate) struct ChooseReq {
    /// a candidate id from this book's cover-candidates
    #[serde(default)]
    candidate: Option<Uuid>,
    /// "embedded": the book's own cover, explicitly
    #[serde(default, rename = "use")]
    use_: Option<String>,
    /// a library pass: refuse (409) when the person already chose
    #[serde(default)]
    unless_chosen: bool,
}

fn already_chosen(b: &Book) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "this book's cover was already chosen; a library pass never overwrites a choice",
            "code": "cover_chosen",
            "book": book_json(b),
        })),
    )
        .into_response()
}

pub(crate) async fn choose_cover(
    State(st): State<LibraryState>,
    v: Viewer,
    Path(id): Path<Uuid>,
    body: Result<Json<ChooseReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let user = match person(&st, &v) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(s3) = st.s3.clone() else { return not_found() };
    let req = match crate::library::json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let by = v.human.to_string();
    let now = crate::auth::now();
    let unless = req.unless_chosen;
    match (req.candidate, req.use_.as_deref()) {
        (None, Some("embedded")) => {
            let done = with_store(&st.store, v.scope, move |s| s.book_choose_cover(id, CoverPick::Embedded, unless, &by, now)).await;
            match done {
                Ok((b, replaced)) => {
                    if let Some(old) = replaced {
                        crate::library::discard_objects(&s3, &[&old]).await;
                    }
                    Json(book_json(&b)).into_response()
                }
                Err(StoreError::Forbidden(_)) => conflict_now(&st, &v, id).await,
                Err(e) => fail(e),
            }
        }
        (Some(cid), None) => {
            let got = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<_> {
                Ok((s.book_get(id)?, s.book_cover_candidate(id, cid)?))
            })
            .await;
            let (book, c) = match got {
                Ok(x) => x,
                Err(e) => return fail(e),
            };
            if unless && book.cover_choice != CoverChoice::None {
                return already_chosen(&book);
            }
            let key = format!("library/{user}/{id}.cover-color.{}", c.ext);
            if let Err(e) = s3.put_bytes(&key, &c.image, crate::library::cover_content_type(&c.ext)).await {
                return err(StatusCode::BAD_GATEWAY, format!("storing the cover: {e:#}"));
            }
            let k = key.clone();
            let done = with_store(&st.store, v.scope, move |s| s.book_choose_cover(id, CoverPick::Color(&k), unless, &by, now)).await;
            match done {
                Ok((b, replaced)) => {
                    if let Some(old) = replaced {
                        crate::library::discard_objects(&s3, &[&old]).await;
                    }
                    Json(book_json(&b)).into_response()
                }
                Err(e) => {
                    // the object at `key` is referenced only if it was already
                    // the colour cover (a same-extension overwrite)
                    if book.color_cover_s3_key.as_deref() != Some(key.as_str()) {
                        crate::library::discard_objects(&s3, &[&key]).await;
                    }
                    match e {
                        StoreError::Forbidden(_) => conflict_now(&st, &v, id).await,
                        e => fail(e),
                    }
                }
            }
        }
        _ => err(StatusCode::BAD_REQUEST, "send {\"candidate\": \"<id>\"} or {\"use\": \"embedded\"}"),
    }
}

/// The 409 for a choice that landed between the check and the write.
async fn conflict_now(st: &LibraryState, v: &Viewer, id: Uuid) -> Response {
    match with_store(&st.store, v.scope, move |s| s.book_get(id)).await {
        Ok(b) => already_chosen(&b),
        Err(e) => fail(e),
    }
}

/// The review list: every greyscale book nobody chose a cover for, then
/// every book with no cover at all (`kind` "greyscale" | "coverless"),
/// oldest first within each, each with its best cached candidate
/// (`looked_up` false = ask POST /cover-candidates), and how many covers
/// are not looked at yet.
async fn review_json(st: &LibraryState, v: &Viewer, user: Uuid, scanned: Option<usize>) -> Response {
    let got = with_store(&st.store, v.scope, move |s| -> taisce_store::Result<_> {
        let (grey, unscanned, coverless) = s.book_cover_review_counts(user)?;
        let mut rows = Vec::new();
        for (kind, books) in [("greyscale", s.book_list_cover_review(user, false, 500)?), ("coverless", s.book_list_coverless(user, 500)?)] {
            for b in books {
                let best = s.book_cover_candidates(b.id)?.into_iter().next();
                rows.push((kind, b, best));
            }
        }
        Ok((grey, unscanned, coverless, rows))
    })
    .await;
    match got {
        Ok((grey, unscanned, coverless, rows)) => {
            let mut out = json!({
                "greyscale": grey,
                "unscanned": unscanned,
                "coverless": coverless,
                "books": rows.iter().map(|(kind, b, best)| json!({
                    "kind": kind,
                    "book": book_json(b),
                    "looked_up": lookup_current(b, best.is_some()),
                    "best": best.as_ref().map(|c| candidate_json(b.id, c)),
                })).collect::<Vec<_>>(),
            });
            if let Some(n) = scanned {
                out["scanned"] = json!(n);
            }
            Json(out).into_response()
        }
        Err(e) => fail(e),
    }
}

pub(crate) async fn review_get(State(st): State<LibraryState>, v: Viewer) -> Response {
    match person(&st, &v) {
        Ok(user) => review_json(&st, &v, user, None).await,
        Err(r) => r,
    }
}

/// Look at the next [`SCAN_BATCH`] covers shelved before greyscale
/// detection (one S3 GET and a small decode each, one at a time), then
/// answer the review. The UI repeats until `unscanned` is 0.
pub(crate) async fn review_scan(State(st): State<LibraryState>, v: Viewer) -> Response {
    let user = match person(&st, &v) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(s3) = st.s3.clone() else { return not_found() };
    let batch = match with_store(&st.store, v.scope, move |s| s.book_list_cover_review(user, true, SCAN_BATCH)).await {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let mut scanned = 0usize;
    for b in batch {
        let Some(key) = b.cover_s3_key.clone() else { continue };
        let verdict = match s3.get_bytes(&key, MAX_STORED_COVER_BYTES).await {
            Ok(Some(bytes)) => blocking(move || cover_verdict(&bytes)).await,
            // the object is gone: nothing to judge, never offered
            Ok(None) => false,
            Err(e) => {
                tracing::warn!(book = %b.id, "colour covers: reading a stored cover: {e:#}");
                if scanned == 0 {
                    return err(StatusCode::BAD_GATEWAY, "object storage is unavailable");
                }
                break;
            }
        };
        let id = b.id;
        if let Err(e) = with_store(&st.store, v.scope, move |s| s.book_set_cover_facts(id, Some(verdict), None)).await {
            // deleted meanwhile: skip it
            if !matches!(e, StoreError::NotFound(_)) {
                return fail(e);
            }
        }
        scanned += 1;
    }
    review_json(&st, &v, user, Some(scanned)).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A JPEG of `w`×`h`: a colour gradient, or the same in grey.
    pub(crate) fn jpeg(w: u32, h: u32, colour: bool) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            let b = 255 - r / 2;
            if colour {
                image::Rgb([r, g, b])
            } else {
                let l = ((r as u32 * 3 + g as u32 * 6 + b as u32) / 10) as u8;
                image::Rgb([l, l, l])
            }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_grey_jpeg_is_greyscale_and_a_colour_one_is_not() {
        let grey = analyse(&jpeg(320, 480, false)).unwrap();
        let colour = analyse(&jpeg(320, 480, true)).unwrap();
        assert_eq!((grey.width, grey.height), (320, 480));
        assert!(grey.colorfulness < 3.0, "{grey:?}");
        assert!(is_greyscale(&grey));
        assert!(colour.colorfulness > COLOUR_MIN, "{colour:?}");
        assert!(!is_greyscale(&colour));
        assert!(cover_verdict(&jpeg(64, 96, false)));
        assert!(!cover_verdict(&jpeg(64, 96, true)));
        // a faint warm tint (an old scan, a cream page) is still greyscale
        let tinted = image::RgbImage::from_fn(100, 150, |_, y| {
            let l = (y * 255 / 150) as f64;
            image::Rgb([l as u8, (l * 0.97) as u8, (l * 0.93) as u8])
        });
        let mut b = std::io::Cursor::new(Vec::new());
        tinted.write_to(&mut b, image::ImageFormat::Png).unwrap();
        assert!(cover_verdict(&b.into_inner()), "a faint tint counts as greyscale");
        // an SVG (or junk) cannot be judged: never "greyscale"
        assert!(analyse(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_none());
        assert!(!cover_verdict(b"<svg/>"));
    }

    #[test]
    fn candidates_must_decode_be_cover_shaped_tall_and_colourful() {
        assert!(validate_candidate(&jpeg(330, 500, true)).is_ok());
        assert_eq!(validate_candidate(&jpeg(330, 500, true)).unwrap().1, "jpg");
        assert!(validate_candidate(&jpeg(330, 500, false)).unwrap_err().contains("not colourful"));
        assert!(validate_candidate(&jpeg(1, 1, true)).unwrap_err().contains("too small"), "the 1×1 placeholder");
        assert!(validate_candidate(&jpeg(180, 280, true)).unwrap_err().contains("too small"));
        assert!(validate_candidate(&jpeg(600, 400, true)).unwrap_err().contains("cover-shaped"));
        assert!(validate_candidate(b"GIF89a junk").is_err());
        assert!(validate_candidate(b"<html>not an image</html>").is_err());
    }

    #[test]
    fn isbns_come_from_identifiers_that_say_so_and_check_out() {
        let ids = |v: &[(Option<&str>, &str)]| v.iter().map(|(s, t)| (s.map(String::from), t.to_string())).collect::<Vec<_>>();
        let got = isbns_from_identifiers(&ids(&[
            (Some("calibre"), "6f1c9a0e-1111-2222-3333-444455556666"),
            (None, "urn:isbn:9781849706186"),
            (Some("ISBN"), "1-84970-618-2"),
            (None, "isbn:0441172717"),
            (Some("MOBI-ASIN"), "B004XJRQUQ"),
            (None, "9780307749710"),
            (None, "urn:isbn:9781849706187"),
            (None, "0441172717"),
            (Some("ISBN"), "978-0-441-17271-9"),
        ]));
        assert_eq!(
            got,
            [
                "9781849706186", // urn:isbn
                // 1-84970-618-2 is the same book's ISBN-10: deduplicated
                "9780441172719", // isbn: prefix, ISBN-10 → 13 (also the last row)
                "9780307749710", // a bare ISBN-13
            ],
            "a bad check digit and a bare 10-digit number are not ISBNs"
        );
        assert_eq!(isbn10_to_13("184970618X".to_lowercase().as_str()).len(), 13);
        assert!(isbn10_valid("080442957x"));
        assert!(isbns_from_identifiers(&[]).is_empty());
    }

    #[test]
    fn titles_match_through_series_tags_subtitles_and_articles() {
        assert_eq!(norm_title("Horus Rising (The Horus Heresy Book 1)"), "horus rising");
        assert_eq!(norm_title("The Hobbit: or There and Back Again"), "hobbit");
        assert!(title_matches("Horus Rising (The Horus Heresy Book 1)", "Horus Rising"));
        assert!(title_matches("Foundation", "Foundation"));
        assert!(title_matches("Horus Rising - The Horus Heresy, Book 1", "Horus Rising"));
        assert!(title_matches("the foundation", "Foundation"));
        assert!(!title_matches("Foundation", "Second Foundation"));
        assert!(!title_matches("Foundation", "Foundation and Empire"), "never a prefix");
        assert!(author_matches("Dan Abnett", &["Abnett, Dan".into()]));
        assert!(!author_matches("Dan Abnett", &["Graham McNeill".into()]));
        assert!(author_matches("", &["Anyone".into()]), "no author to check");
    }

    /// Series brackets are stripped on BOTH sides; Open Library's own
    /// series prefix counts only when it is the book's series.
    #[test]
    fn series_brackets_and_prefixes_match_only_the_books_own_series() {
        assert_eq!(strip_groups("Saturnine (The Horus Heresy: Siege of Terra Book 4)"), "Saturnine");
        assert_eq!(strip_groups("Red Tithe (Carcharodons Book 1) [Kindle]"), "Red Tithe");
        assert_eq!(strip_groups("(Untitled)"), "(Untitled)", "nothing would remain");
        assert_eq!(strip_groups("Night Lords: The Omnibus"), "Night Lords: The Omnibus");
        assert!(title_matches("Red Tithe (Carcharodons Book 1)", "Red Tithe (Carcharodons)"));
        assert!(title_matches("Red Tithe", "Red Tithe (Carcharodons)"));
        assert!(title_matches("Betrayer (The Horus Heresy Book 24)", "Betrayer"));
        assert!(title_matches("Saturnine (The Horus Heresy: Siege of Terra Book 4)", "Saturnine"));
        assert!(!title_matches("Foundation (Foundation Book 1)", "Foundation and Empire (Foundation Book 2)"));
        assert_eq!(series_words("Helsreach (Space Marine Battles)"), ["space", "marine", "battles"]);
        assert_eq!(series_words("Betrayer (The Horus Heresy Book 24)"), ["horus", "heresy"]);
        let h = "Helsreach (Space Marine Battles)";
        assert_eq!(title_match(h, "Space Marine Battles nº 01/04 Helsreach"), Some(TitleMatch::SeriesPrefixed));
        assert_eq!(title_match(h, "Space Marine Battles Helsreach"), Some(TitleMatch::SeriesPrefixed));
        assert_eq!(title_match(h, "Helsreach"), Some(TitleMatch::Exact));
        assert_eq!(title_match(h, "Space Marine Battles nº 02/04 Rynn's World"), None, "another book of the series");
        assert_eq!(title_match(h, "Space Marine Battles nº 01/04 Helsreach and More"), None, "still exact after the prefix");
        assert_eq!(title_match("Helsreach", "Space Marine Battles nº 01/04 Helsreach"), None, "no series of ours to strip");
        assert_eq!(title_match("Helsreach (Black Library)", "Space Marine Battles Helsreach"), None, "not our series");
    }

    #[test]
    fn searches_go_from_the_full_title_to_the_surname() {
        let v = |t: &str, a: &str| search_variants(t, a).into_iter().map(|(t, a)| format!("{t} | {a}")).collect::<Vec<_>>();
        assert_eq!(
            v("Saturnine (The Horus Heresy: Siege of Terra Book 4)", "Dan Abnett"),
            [
                "Saturnine (The Horus Heresy: Siege of Terra Book 4) | Dan Abnett",
                "Saturnine | Dan Abnett",
                "Saturnine | Abnett",
            ]
        );
        assert_eq!(
            v("Night Lords: The Omnibus", "Aaron Dembski-Bowden"),
            ["Night Lords: The Omnibus | Aaron Dembski-Bowden", "Night Lords | Aaron Dembski-Bowden", "Night Lords: The Omnibus | Dembski-Bowden"]
        );
        assert_eq!(v(": Omnibus", ""), [": Omnibus | "], "no subtitle cut leaves nothing; no author, no surname");
        assert_eq!(v("Foundation", "Asimov"), ["Foundation | Asimov"], "duplicates once");
        assert_eq!(surname("Abnett, Dan").as_deref(), Some("Abnett"));
        assert_eq!(surname("Graham McNeill & Dan Abnett").as_deref(), Some("McNeill"));
        assert_eq!(surname("Isaac Asimov and Robert Silverberg").as_deref(), Some("Asimov"));
        assert_eq!(surname(""), None);
    }

    #[test]
    fn urls_are_built_by_the_server_and_only_open_library_is_fetched() {
        assert_eq!(OpenLibrary::isbn_cover_url("9781849706186").as_str(), "https://covers.openlibrary.org/b/isbn/9781849706186-L.jpg?default=false");
        assert_eq!(OpenLibrary::id_cover_url(894713).as_str(), "https://covers.openlibrary.org/b/id/894713-L.jpg?default=false");
        assert_eq!(
            OpenLibrary::search_url("Horus Rising ", "Dan Abnett").as_str(),
            "https://openlibrary.org/search.json?title=Horus+Rising&author=Dan+Abnett&limit=5&fields=key%2Ctitle%2Cauthor_name%2Ccover_i%2Cisbn%2Cfirst_publish_year"
        );
        // a title cannot smuggle a path or a host
        let u = OpenLibrary::search_url("../../x?y=1#z", "a&b=c");
        assert_eq!((u.host_str(), u.path()), (Some("openlibrary.org"), "/search.json"));
        for ok in ["https://covers.openlibrary.org/b/id/1-L.jpg", "https://openlibrary.org/search.json"] {
            assert!(start_allowed(&Url::parse(ok).unwrap()), "{ok}");
        }
        for bad in [
            "http://covers.openlibrary.org/b/id/1-L.jpg",
            "https://covers.openlibrary.org:8443/b/id/1-L.jpg",
            "https://archive.org/download/x",
            "https://169.254.169.254/latest/meta-data/",
            "https://openlibrary.org.evil.example/x",
            "https://evil.example/?h=openlibrary.org",
        ] {
            assert!(!start_allowed(&Url::parse(bad).unwrap()), "{bad}");
        }
        // redirects: Open Library's, then the Internet Archive's nodes
        for ok in [
            "https://archive.org/download/olcovers89/olcovers89-L.zip/894713-L.jpg",
            "https://ia800703.us.archive.org/view_archive.php?archive=/15/items/olcovers89/olcovers89-L.zip&file=894713-L.jpg",
        ] {
            assert!(redirect_allowed(&Url::parse(ok).unwrap()), "{ok}");
        }
        for bad in ["http://archive.org/x", "https://evil.us.archive.org.example/x", "https://localhost/x", "https://10.0.0.1/x", "https://x.archive.org/y"] {
            assert!(!redirect_allowed(&Url::parse(bad).unwrap()), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_client_supplied_url_is_never_fetched() {
        let ol = OpenLibrary::for_test();
        let r = ol.get(Url::parse("https://169.254.169.254/latest/meta-data/").unwrap()).await;
        assert!(r.unwrap_err().contains("not Open Library"));
        assert!(ol.mock.asked.lock().unwrap().is_empty(), "nothing was sent");
    }

    /// The lookup: the ISBN's cover first, then matching search results;
    /// the placeholder, a grey scan, a non-matching title and a duplicate
    /// are dropped; at most MAX_CANDIDATES come back.
    #[tokio::test]
    async fn a_lookup_ranks_isbn_first_and_validates_every_image() {
        let ol = OpenLibrary::for_test();
        let m = ol.mock.clone();
        let colour = jpeg(330, 500, true);
        m.answer(OpenLibrary::isbn_cover_url("9781849706186").as_str(), 200, colour.clone());
        m.answer(
            OpenLibrary::search_url("Horus Rising (The Horus Heresy Book 1)", "Dan Abnett").as_str(),
            200,
            serde_json::to_vec(&json!({"docs": [
                {"title": "Horus Rising", "author_name": ["Dan Abnett"], "cover_i": 1},
                {"title": "Horus Rising", "author_name": ["Dan Abnett"], "cover_i": 2},
                {"title": "Horus Rising", "author_name": ["Dan Abnett"], "cover_i": 3},
                {"title": "False Gods", "author_name": ["Graham McNeill"], "cover_i": 4},
                {"title": "Horus Rising", "author_name": ["Dan Abnett"], "cover_i": 5},
                {"title": "Horus Rising", "author_name": ["Dan Abnett"]},
            ]}))
            .unwrap(),
        );
        m.answer(OpenLibrary::id_cover_url(1).as_str(), 200, colour.clone()); // the ISBN's again
        m.answer(OpenLibrary::id_cover_url(2).as_str(), 200, jpeg(330, 500, false)); // a grey scan
        m.answer(OpenLibrary::id_cover_url(3).as_str(), 200, jpeg(300, 480, true));
        m.answer(OpenLibrary::id_cover_url(4).as_str(), 200, jpeg(310, 500, true));
        m.answer(OpenLibrary::id_cover_url(5).as_str(), 200, jpeg(320, 500, true));
        let got = ol.lookup("Horus Rising (The Horus Heresy Book 1)", "Dan Abnett", &["9781849706186".into()]).await;
        assert!(!got.unavailable);
        let refs: Vec<(&str, &str)> = got.candidates.iter().map(|c| (c.source.as_str(), c.source_ref.as_str())).collect();
        assert_eq!(refs, [("isbn", "9781849706186"), ("search", "3"), ("search", "5")]);
        let asked = m.asked.lock().unwrap().clone();
        assert!(!asked.iter().any(|u| u.contains("/b/id/4-")), "another book's cover is never fetched: {asked:?}");
        assert!(asked.iter().all(|u| u.starts_with("https://covers.openlibrary.org/") || u.starts_with("https://openlibrary.org/")));
    }

    #[tokio::test]
    async fn a_failing_open_library_is_unavailable_not_empty() {
        let ol = OpenLibrary::for_test();
        ol.mock.answer(OpenLibrary::search_url("Foundation", "Isaac Asimov").as_str(), 503, "");
        let got = ol.lookup("Foundation", "Isaac Asimov", &[]).await;
        assert!(got.candidates.is_empty() && got.unavailable);
        // nothing anywhere (every URL a 404) is a real "none found"
        let got = ol.lookup("Nothing Like It", "Nobody", &["9780307749710".into()]).await;
        assert!(got.candidates.is_empty() && !got.unavailable);
    }

    fn docs(v: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({ "docs": v })).unwrap()
    }

    /// Tom's Warhammer 40k books, as Open Library answered them live: the
    /// title with its series in brackets finds nothing (or a Spanish
    /// edition), the bare title finds the book. Each comes through the
    /// stripped search; the full one was asked first.
    #[tokio::test]
    async fn series_bracketed_titles_match_through_the_stripped_search() {
        let cases: [(&str, &str, &str, Value, Vec<i64>); 4] = [
            (
                "Red Tithe (Carcharodons Book 1)",
                "Robbie MacNiven",
                "Red Tithe",
                json!([{"title": "Red Tithe (Carcharodons)", "author_name": ["Robbie MacNiven"], "cover_i": 8446358}]),
                vec![8446358],
            ),
            (
                "Betrayer (The Horus Heresy Book 24)",
                "Aaron Dembski-Bowden",
                "Betrayer",
                json!([
                    {"title": "Betrayer", "author_name": ["Aaron Dembski-Bowden"], "cover_i": 14841144},
                    {"title": "Betrayer", "author_name": ["Aaron Dembski-Bowden"], "cover_i": 10374176},
                    {"title": "Legacies of Betrayal", "author_name": ["Graham McNeill", "Aaron Dembski-Bowden"], "cover_i": 99},
                ]),
                vec![14841144, 10374176],
            ),
            (
                "Saturnine (The Horus Heresy: Siege of Terra Book 4)",
                "Dan Abnett",
                "Saturnine",
                json!([
                    {"title": "Saturnine", "author_name": ["Dan Abnett"], "cover_i": 10413534},
                    {"title": "Saturnine", "author_name": ["Dan Abnett"]},
                    {"title": "Saturnine", "author_name": ["Dan Abnett"], "cover_i": 14841168},
                ]),
                vec![10413534, 14841168],
            ),
            (
                "Helsreach (Space Marine Battles)",
                "Aaron Dembski-Bowden",
                "Helsreach",
                json!([
                    {"title": "Helsreach", "author_name": ["Aaron Dembski-Bowden"], "cover_i": 7741218},
                    {"title": "Helsreach", "author_name": ["Aaron Dembski-Bowden"], "cover_i": 9126586},
                ]),
                // the Spanish edition the full title found ranks last
                vec![7741218, 9126586, 14120028],
            ),
        ];
        for (title, author, stripped, found, want) in cases {
            let ol = OpenLibrary::for_test();
            let m = ol.mock.clone();
            let full = if title.starts_with("Helsreach") {
                json!([{"title": "Space Marine Battles nº 01/04 Helsreach", "author_name": ["Aaron Dembski-Bowden", "Vicky Charques"], "cover_i": 14120028}])
            } else {
                json!([])
            };
            m.answer(OpenLibrary::search_url(title, author).as_str(), 200, docs(full));
            m.answer(OpenLibrary::search_url(stripped, author).as_str(), 200, docs(found));
            for (i, id) in [8446358i64, 14841144, 10374176, 10413534, 14841168, 7741218, 9126586, 14120028, 99].into_iter().enumerate() {
                m.answer(OpenLibrary::id_cover_url(id).as_str(), 200, jpeg(320 + i as u32, 500, true));
            }
            let got = ol.lookup(title, author, &[]).await;
            assert!(!got.unavailable, "{title}");
            let refs: Vec<i64> = got.candidates.iter().map(|c| c.source_ref.parse().unwrap()).collect();
            assert_eq!(refs, want, "{title}");
            let asked: Vec<String> = m.asked.lock().unwrap().iter().filter(|u| u.contains("search.json")).cloned().collect();
            assert_eq!(
                asked,
                [OpenLibrary::search_url(title, author).to_string(), OpenLibrary::search_url(stripped, author).to_string()],
                "{title}: the full title first, and the search stops at the first exact match"
            );
        }
    }

    /// Every variant is tried for Foundation, and none of them lets it take
    /// Foundation and Empire's cover.
    #[tokio::test]
    async fn foundation_never_takes_foundation_and_empire() {
        let ol = OpenLibrary::for_test();
        let m = ol.mock.clone();
        let other = docs(json!([{"title": "Foundation and Empire", "author_name": ["Isaac Asimov"], "cover_i": 5}]));
        for (t, a) in search_variants("Foundation (Foundation Book 1)", "Isaac Asimov") {
            m.answer(OpenLibrary::search_url(&t, &a).as_str(), 200, other.clone());
        }
        m.answer(OpenLibrary::id_cover_url(5).as_str(), 200, jpeg(330, 500, true));
        let got = ol.lookup("Foundation (Foundation Book 1)", "Isaac Asimov", &[]).await;
        assert!(got.candidates.is_empty() && !got.unavailable);
        let asked = m.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 3, "full, stripped, surname: {asked:?}");
        assert!(asked.iter().all(|u| u.contains("search.json")), "the other book's cover is never fetched: {asked:?}");
        assert!(asked[2].contains("title=Foundation&author=Asimov"), "{asked:?}");
    }

    /// The surname-only search runs last, when everything else found nothing.
    #[tokio::test]
    async fn the_surname_alone_is_the_last_try() {
        let ol = OpenLibrary::for_test();
        let m = ol.mock.clone();
        m.answer(
            OpenLibrary::search_url("Helsreach", "Dembski-Bowden").as_str(),
            200,
            docs(json!([{"title": "Helsreach", "author_name": ["Aaron Dembski-Bowden"], "cover_i": 7}])),
        );
        m.answer(OpenLibrary::id_cover_url(7).as_str(), 200, jpeg(330, 500, true));
        let got = ol.lookup("Helsreach (Space Marine Battles)", "Dembski-Bowden, Aaron", &[]).await;
        assert_eq!(got.candidates.iter().map(|c| c.source_ref.as_str()).collect::<Vec<_>>(), ["7"]);
    }

    fn stamped(at: Option<i64>, matcher: Option<i64>) -> Book {
        Book {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            title: "t".into(),
            author: "a".into(),
            series: None,
            series_index: None,
            book_identifier: None,
            original_filename: "f.epub".into(),
            file_sha256: "s".into(),
            size: 1,
            file_s3_key: "k".into(),
            cover_s3_key: None,
            added_at: 0,
            updated_at: 0,
            deleted_at: None,
            cover_grey: Some(true),
            isbns: None,
            color_cover_s3_key: None,
            cover_choice: CoverChoice::None,
            cover_choice_by: None,
            cover_choice_at: None,
            color_lookup_at: at,
            color_lookup_matcher: matcher,
        }
    }

    /// A miss cached by an older matcher is stale; one by this matcher, or
    /// any lookup that found candidates, is current.
    #[test]
    fn an_older_matchers_miss_is_stale() {
        assert!(!lookup_current(&stamped(None, None), false), "never looked up");
        assert!(!lookup_current(&stamped(Some(1), None), false), "an unstamped (v1) miss");
        assert!(!lookup_current(&stamped(Some(1), Some(MATCHER_VERSION - 1)), false));
        assert!(lookup_current(&stamped(Some(1), Some(MATCHER_VERSION)), false));
        assert!(lookup_current(&stamped(Some(1), None), true), "an old lookup's candidates still stand");
    }

    /// Live misses from Tom's library, with Open Library's answers as
    /// they came (2026-10-23): each now finds its cover.
    #[tokio::test]
    async fn the_live_misses_match() {
        let pad = format!("The First Heretic{}Warhammer 40000 Novels Horus Heresy", " ".repeat(28));
        let cases: Vec<(&str, &str, &str, Value, Vec<i64>)> = vec![
            (
                "Vulkan Lives",
                "Nick Kyme",
                "Vulkan Lives",
                json!([
                    {"title": "Vulkan Lives", "author_name": ["Nick Kyme"], "cover_i": 10214975},
                    {"title": "Vulkan Lives", "author_name": ["Nick Kyme"], "cover_i": 13718144},
                ]),
                vec![10214975, 13718144],
            ),
            (
                "The First Heretic (The Horus Heresy Book 14)",
                "Aaron Dembski-Bowden",
                "The First Heretic",
                json!([{"title": pad, "author_name": ["Aaron Dembski-Bowden"], "cover_i": 7724581}]),
                vec![7724581],
            ),
            (
                "2001: A Space Odyssey",
                "Arthur C. Clarke",
                "2001: A Space Odyssey",
                json!([
                    {"title": "2001 A SPACE ODYSSEY", "author_name": ["Arthur C. Clarke"], "cover_i": 15170836},
                    {"title": "2001, a space odyssey ; The city and the stars ; The deep range ; A fall of moondust ; Rendezvous with Rama", "author_name": ["Arthur C. Clarke"], "cover_i": 12134736},
                    {"title": "2001; a space odyssey", "author_name": ["Arthur C. Clarke"]},
                ]),
                vec![15170836],
            ),
            (
                // Open Library writes the author in Chinese and truncates the title
                "Death's End (The Three-Body Problem)",
                "Cixin Liu",
                "Death's End",
                json!([
                    {"title": "Death's End (The Three-Body Pr", "author_name": ["刘慈欣"], "cover_i": 7893958},
                    {"title": "Cixin Liu Bestselling Collecting Books Series, Set of 4 Books. the Three-Body Problem, the Wandering Earth, the Dark Forest and Death's End", "author_name": ["Cixin Liu"], "cover_i": 1},
                ]),
                vec![7893958],
            ),
            (
                // results without a cover are skipped
                "After Dark",
                "Haruki Murakami",
                "After Dark",
                json!([
                    {"title": "After Dark", "author_name": ["村上春樹"]},
                    {"title": "After dark", "author_name": ["村上春樹", "Albert Nolla Cabellos"], "cover_i": 13524033},
                    {"title": "After Dark", "author_name": ["村上春樹"], "cover_i": null},
                    {"title": "AFTER DARK", "author_name": ["村上春樹"], "cover_i": 12326861},
                ]),
                vec![13524033, 12326861],
            ),
            (
                // our data's curly apostrophe, Open Library's straight one
                "Foundation\u{2019}s Edge",
                "Isaac Asimov",
                "Foundation\u{2019}s Edge",
                json!([
                    {"title": "Foundation's Edge", "author_name": ["Isaac Asimov"], "cover_i": 6700720},
                    {"title": "Novels (Foundation / Foundation and Empire / Foundation's Edge / Second Foundation)", "author_name": ["Isaac Asimov"], "cover_i": 6893876},
                ]),
                vec![6700720],
            ),
        ];
        for (title, author, query, found, want) in cases {
            let ol = OpenLibrary::for_test();
            let m = ol.mock.clone();
            if query != title {
                m.answer(OpenLibrary::search_url(title, author).as_str(), 200, docs(json!([])));
            }
            m.answer(OpenLibrary::search_url(query, author).as_str(), 200, docs(found));
            for (i, id) in [10214975i64, 13718144, 7724581, 15170836, 12134736, 7893958, 1, 13524033, 12326861, 6700720, 6893876].into_iter().enumerate() {
                m.answer(OpenLibrary::id_cover_url(id).as_str(), 200, jpeg(320 + i as u32, 500, true));
            }
            let got = ol.lookup(title, author, &[]).await;
            let refs: Vec<i64> = got.candidates.iter().map(|c| c.source_ref.parse().unwrap()).collect();
            assert_eq!(refs, want, "{title}");
        }
        assert!(title_matches("Foundation's Edge", "Foundation\u{2019}s Edge"));
        // another script is a weak match; a different Latin name is none
        assert_eq!(author_match("Cixin Liu", &["刘慈欣".into()]), Some(false));
        assert_eq!(author_match("Haruki Murakami", &["Albert Nolla Cabellos".into()]), None);
        assert_eq!(author_match("Haruki Murakami", &["Albert Nolla Cabellos".into(), "村上春樹".into()]), Some(false));
        assert!(!title_matches("2001: A Space Odyssey", "2001, a space odyssey ; The city and the stars"));
        assert_eq!(padded_head("   Dune   "), "Dune");
        assert_eq!(padded_head("Dune    Messiah"), "Dune");
    }
}
