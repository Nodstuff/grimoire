//! Derived book filenames: the `<Author> - <Title>.epub` both OPDS clients
//! compute on device, and the kosync digest keyed on it.
//!
//! Neither known client uses a server-chosen name: both name a download from
//! the FEED's `<title>` and `<author><name>`, then sanitize. Verified from
//! their source:
//!
//! - CrossPoint (`src/util/OpdsFilename.cpp`, `StringUtils.cpp`
//!   `sanitizeFilenameRange`, default AuthorTitle format): base = `author +
//!   " - " + title` (title alone when the author is empty); skip LEADING
//!   spaces and dots; per codepoint `\ / : * ? " < > |` → `_`, keep cp >= 128
//!   and printable ASCII, drop control chars; stop before exceeding 100 BYTES
//!   (never splitting a codepoint); trim TRAILING spaces and dots; empty →
//!   "book"; append ".epub".
//! - KOReader (`opdsbrowser.lua` `getFileName` + `util.replaceAllInvalidChars`
//!   + `getSafeFilename`): `author .. " - " .. title` (title alone when the
//!   entry's author is nil — an empty `<name></name>` parses as an empty
//!   table, which it treats as nil; a MISSING `<author>` becomes the
//!   localized "Unknown Author", so the feed always emits the element), the
//!   same 9 chars → `_`, strip trailing `[.%s]+`, newlines/tabs → space, cap
//!   240 bytes, + ".epub". No leading strip, no 100-byte cap. Its kosync
//!   "Filename" method digests md5(basename incl. extension), lowercase hex.
//!
//! The two agree exactly when the base is ≤ 100 bytes, starts with no space
//! or dot and carries no control characters. [`feed_title`] produces exactly
//! such strings, and the feed emits them, so every sanitizer step is a no-op
//! on both devices apart from the 9-char → `_` replacement they do
//! identically. [`derived_book_filename`] is then the one name the feed, the
//! download's Content-Disposition, the kosync digest and the shelf's clash
//! check all use. The stored title (the Books view, `/api/books`) stays as
//! typed.
//!
//! See Taisce › EPUB proposal › "What the feed serves".

/// CrossPoint's byte budget for the base (KOReader's 240 is looser).
const MAX_BASE_BYTES: usize = 100;
/// " - " between author and title.
const SEP: &str = " - ";
/// An author longer than this is truncated too, so the title keeps room.
const MAX_AUTHOR_BYTES: usize = 90;
/// The title a book with none (after cleaning) is served under.
const UNTITLED: &str = "Untitled";
/// The characters both clients replace with `_`.
const INVALID: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// One label as the feed serves it: control characters (tabs and newlines
/// included) become spaces, whitespace runs collapse to one space, and both
/// ends are trimmed.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// The longest prefix of `s` within `max` bytes, on a char boundary.
fn cap_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The `(title, author)` strings the OPDS feed emits for a book: cleaned
/// (see [`clean`]), the first component's leading spaces/dots dropped (the
/// one sanitizer step the clients do differently), and the base `author +
/// " - " + title` held to 100 bytes by truncating the title on a char
/// boundary (then its trailing spaces/dots), and the author too when it alone
/// would leave the title no room. `author` is "" for a book without one.
pub fn feed_title(title: &str, author: &str) -> (String, String) {
    let mut author = clean(author);
    let mut title = clean(title);
    if author.trim_start_matches([' ', '.']).is_empty() {
        author.clear();
    }
    if author.is_empty() {
        title = title.trim_start_matches([' ', '.']).to_string();
        if title.is_empty() {
            title = UNTITLED.into();
        }
        let t = cap_bytes(&title, MAX_BASE_BYTES).trim_end_matches([' ', '.']);
        return (t.to_string(), author);
    }
    author = author.trim_start_matches([' ', '.']).to_string();
    if title.is_empty() {
        title = UNTITLED.into();
    }
    if author.len() + SEP.len() + title.len() <= MAX_BASE_BYTES {
        return (title, author);
    }
    if author.len() > MAX_AUTHOR_BYTES {
        author = cap_bytes(&author, MAX_AUTHOR_BYTES).trim_end().to_string();
    }
    let budget = MAX_BASE_BYTES - SEP.len() - author.len();
    let mut t = cap_bytes(&title, budget).trim_end_matches([' ', '.']).to_string();
    if t.is_empty() {
        // the title was all spaces/dots inside the budget: keep one char of
        // it so the name still says something (budget ≥ 7 bytes ≥ 1 char)
        t = title.chars().next().map(String::from).unwrap_or_else(|| UNTITLED.into());
    }
    (t, author)
}

/// CrossPoint's `sanitizeFilename(base, 100)`, ported faithfully: on
/// [`feed_title`]'s output only the 9-char replacement does anything.
pub fn sanitize_base(base: &str) -> String {
    let mut out = String::with_capacity(base.len().min(MAX_BASE_BYTES));
    for c in base.trim_start_matches([' ', '.']).chars() {
        let cp = c as u32;
        if INVALID.contains(&c) {
            if out.len() + 1 > MAX_BASE_BYTES {
                break;
            }
            out.push('_');
        } else if cp >= 128 || (32..127).contains(&cp) {
            if out.len() + c.len_utf8() > MAX_BASE_BYTES {
                break;
            }
            out.push(c);
        }
    }
    let trimmed = out.trim_end_matches([' ', '.']);
    if trimmed.is_empty() { "book".to_string() } else { trimmed.to_string() }
}

/// The filename both clients save a feed download as:
/// `sanitize(feed_title(…)) + ".epub"`.
pub fn derived_book_filename(title: &str, author: &str) -> String {
    let (t, a) = feed_title(title, author);
    let base = if a.is_empty() { t } else { format!("{a}{SEP}{t}") };
    format!("{}.epub", sanitize_base(&base))
}

/// kosync's document digest under Filename matching: MD5 of the derived
/// filename (with extension), lowercase hex — what KOReader computes on
/// device for a feed-downloaded book.
pub fn kosync_document_digest(title: &str, author: &str) -> String {
    md5_hex(derived_book_filename(title, author).as_bytes())
}

/// MD5 lowercase hex of the given bytes.
pub fn md5_hex(bytes: &[u8]) -> String {
    format!("{:x}", md5::compute(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CrossPoint end to end (`opdsBookFilename`, AuthorTitle).
    fn crosspoint(author: &str, title: &str) -> String {
        let base = if author.is_empty() { title.to_string() } else { format!("{author} - {title}") };
        format!("{}.epub", sanitize_base(&base))
    }

    /// Lua's `%s` (C locale isspace).
    fn lua_space(c: char) -> bool {
        matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
    }

    /// `util.replaceAllInvalidChars`.
    fn ko_replace_all_invalid(s: &str) -> String {
        let s: String = s.chars().map(|c| if INVALID.contains(&c) { '_' } else { c }).collect();
        s.trim_end_matches(|c: char| c == '.' || lua_space(c)).to_string()
    }

    /// KOReader end to end: `getFileName` (author nil when the feed's name is
    /// empty), then `getLocalDownloadPath` → `getSafeFilename` on a VFAT
    /// download dir (the stricter branch; others only replace '/').
    fn koreader(author: &str, title: &str) -> String {
        let mut f = title.to_string();
        if !author.is_empty() {
            f = format!("{author} - {f}");
        }
        let f = ko_replace_all_invalid(&f) + ".epub";
        // splitFileNameSuffix: greedy (.*)%.(.*)
        let (name, suffix) = f.rsplit_once('.').unwrap();
        let name = name.replace("\r\n", " ").replace('\n', " ").replace('\t', " ");
        // filename:sub(1, 240) then fixUtf8 drops a split trailing codepoint
        let name = cap_bytes(&name, 240);
        format!("{}.{}", ko_replace_all_invalid(name), ko_replace_all_invalid(suffix))
    }

    #[test]
    fn colon_becomes_underscore_on_both() {
        assert_eq!(derived_book_filename("Dune: Messiah", "Frank Herbert"), "Frank Herbert - Dune_ Messiah.epub");
        assert_eq!(
            derived_book_filename("Red Tithe (Carcharodons Book 1)", "Robbie MacNiven"),
            "Robbie MacNiven - Red Tithe (Carcharodons Book 1).epub"
        );
    }

    #[test]
    fn unicode_letters_are_kept() {
        assert_eq!(derived_book_filename("Midsummer Night Madness", "Seán Ó Faoláin"), "Seán Ó Faoláin - Midsummer Night Madness.epub");
        assert_eq!(derived_book_filename("Ficciones", "Jorge Luis Borges"), "Jorge Luis Borges - Ficciones.epub");
    }

    #[test]
    fn empty_author_is_title_alone() {
        assert_eq!(derived_book_filename("After Dark", ""), "After Dark.epub");
        assert_eq!(derived_book_filename("After Dark", "  .. "), "After Dark.epub");
        assert_eq!(feed_title("After Dark", ""), ("After Dark".into(), "".into()));
    }

    #[test]
    fn long_titles_cap_the_base_at_100_bytes_on_a_char_boundary() {
        // 150 bytes of two-byte chars: the cut must land between them
        let title = "é".repeat(75);
        let (t, a) = feed_title(&title, "Frank Herbert");
        let base = format!("{a} - {t}");
        assert!(base.len() <= 100, "{}", base.len());
        assert_eq!(base.len(), 100, "Frank Herbert - (16 bytes) + 42 é (84 bytes)");
        assert!(t.chars().all(|c| c == 'é'));
        let f = derived_book_filename(&title, "Frank Herbert");
        assert_eq!(f, format!("{base}.epub"));
        // no author: the title alone gets the whole budget
        let (t, _) = feed_title(&"x".repeat(150), "");
        assert_eq!(t.len(), 100);
        // truncation never leaves a trailing space or dot
        let (t, a) = feed_title(&format!("{}. tail", "y".repeat(92)), "Ann");
        assert_eq!(t, "y".repeat(92), "{a} - {t}");
    }

    #[test]
    fn a_huge_author_is_truncated_and_the_title_keeps_room() {
        let author = "A".repeat(150);
        let (t, a) = feed_title("Dune", &author);
        assert_eq!(a.len(), MAX_AUTHOR_BYTES);
        assert_eq!(t, "Dune");
        assert!(a.len() + 3 + t.len() <= 100);
    }

    #[test]
    fn leading_dots_and_spaces_and_controls_are_cleaned() {
        assert_eq!(feed_title("  ...Hidden", ""), ("Hidden".into(), "".into()));
        assert_eq!(feed_title(".hack", "  .Kazunori Ito"), (".hack".into(), "Kazunori Ito".into()), "only the FIRST component's leading dots");
        assert_eq!(feed_title("Line\none\ttwo\r\n", "A\u{7f}B"), ("Line one two".into(), "A B".into()));
        assert_eq!(feed_title("...", ""), ("Untitled".into(), "".into()));
        assert_eq!(derived_book_filename("  ...Hidden", ""), "Hidden.epub");
    }

    /// The point of it all: over feed_title's output, a faithful port of each
    /// client's sanitizer produces the SAME filename, and it is ours.
    #[test]
    fn both_clients_derive_the_same_name_from_the_feed() {
        let long = "Ω".repeat(80);
        let cases: &[(&str, &str)] = &[
            ("Dune: Messiah", "Frank Herbert"),
            ("Midsummer Night Madness", "Seán Ó Faoláin"),
            ("After Dark", ""),
            ("  ...Hidden", ""),
            (".hack//Sign", "Kazunori Itō"),
            ("Who? What? <Where>", "\"Quoted\" | Piped"),
            ("Tabs\tand\nnewlines\r\n", "Some\u{0}One"),
            ("The Hobbit, or There and Back Again", "J. R. R. Tolkien"),
            ("Ends with dots...", "Author."),
            (&long, "Frank Herbert"),
            (&long, ""),
            ("Dune", &long),
            ("x", "   "),
            ("", "Nobody"),
            ("百年の孤独 — 長い題名がここに続きます。さらに続きます。まだ続きます。もっと続きます。", "ガブリエル・ガルシア＝マルケス"),
            ("Emoji 📚 shelf", "🦀 Ferris"),
        ];
        for (title, author) in cases {
            let (t, a) = feed_title(title, author);
            let ours = derived_book_filename(title, author);
            assert_eq!(crosspoint(&a, &t), ours, "CrossPoint on {title:?}/{author:?}");
            assert_eq!(koreader(&a, &t), ours, "KOReader on {title:?}/{author:?}");
            let base = ours.strip_suffix(".epub").unwrap();
            assert!(base.len() <= 100, "{ours}");
        }
    }

    #[test]
    fn digest_is_md5_of_the_derived_filename() {
        let d = kosync_document_digest("Piranesi", "Susanna Clarke");
        assert_eq!(d, md5_hex(b"Susanna Clarke - Piranesi.epub"));
        assert_eq!(d.len(), 32);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
