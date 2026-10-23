//! Derived book filenames: the canonical `<Author> - <Title>.epub` scheme.
//!
//! Both known OPDS clients name downloads from feed metadata, not from any
//! server-chosen name: KOReader's `getFileName` and CrossPoint's
//! `opdsBookFilename` both default to `<Author> - <Title>.epub`, sanitized.
//! We therefore normalize title/author at ingest into a character set both
//! sanitizers leave alone, inside CrossPoint's 100-byte budget, so every
//! client derives the same name. Filename matching (kosync) makes the name a
//! sync key, so the digest helpers live here too.
//!
//! See Taisce › EPUB proposal › "What the feed serves".

/// Compute the derived filename base (no extension) for a book, then the
/// full filename via [`derived_book_filename`].
pub fn derived_book_filename(title: &str, author: &str) -> String {
    format!("{}.epub", derived_base(title, author))
}

/// The `<Author> - <Title>` base: normalized, capped at 100 bytes on a UTF-8
/// boundary, never empty (falls back to "book" like CrossPoint).
pub fn derived_base(title: &str, author: &str) -> String {
    let raw = if author.trim().is_empty() {
        normalize(title)
    } else {
        format!("{} - {}", normalize(author), normalize(title))
    };
    let capped = cap_bytes(raw.trim(), 100);
    if capped.is_empty() {
        "book".to_string()
    } else {
        capped
    }
}

/// kosync's document digest under Filename matching: MD5 of the filename
/// (with extension), lowercase hex — identical to what KOReader computes on
/// device for a feed-downloaded book.
pub fn kosync_document_digest(title: &str, author: &str) -> String {
    md5_hex(derived_book_filename(title, author).as_bytes())
}

/// MD5 lowercase hex of the given bytes (small, dependency-free wrapper;
/// the daemon already vendors an md5 for APNs/HMAC work — if a shared helper
/// exists, prefer it and delete this).
pub fn md5_hex(bytes: &[u8]) -> String {
    let digest = md5::compute(bytes);
    format!("{:x}", digest)
}

/// Conservative charset: keep ASCII letters/digits and ` -'.,()&`, collapse
/// everything else to a single space. Both client sanitizers are no-ops on
/// this set, so the derived name survives intact on both devices.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true; // trim leading
    for ch in s.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '\'' | '.' | ',' | '(' | ')' | '&');
        if ok {
            if ch == ' ' {
                if last_was_space {
                    continue;
                }
                last_was_space = true;
            } else {
                last_was_space = false;
            }
            out.push(ch);
        } else {
            // Drop disallowed characters entirely rather than inventing a
            // replacement both sanitizers would then treat differently.
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        }
    }
    while out.ends_with(' ') || out.ends_with('-') {
        out.pop();
    }
    out.trim().to_string()
}

fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let capped = s[..end].trim_end_matches([' ', '-', '.']);
    capped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_is_author_dash_title() {
        assert_eq!(
            derived_book_filename("Red Tithe (Carcharodons Book 1)", "Robbie MacNiven"),
            "Robbie MacNiven - Red Tithe (Carcharodons Book 1).epub"
        );
    }

    #[test]
    fn empty_author_uses_title_only() {
        assert_eq!(derived_book_filename("After Dark", ""), "After Dark.epub");
    }

    #[test]
    fn disallowed_chars_drop_out() {
        assert_eq!(
            derived_base("Echoes: Of/Eternity?", "Dembski-Bowden"),
            "Dembski-Bowden - Echoes Of Eternity"
        );
    }

    #[test]
    fn long_names_cap_at_100_bytes() {
        let long = "x".repeat(200);
        assert_eq!(derived_base(&long, "a").len(), 100);
    }

    #[test]
    fn never_empty() {
        assert_eq!(derived_base("!!!", ""), "book");
    }

    #[test]
    fn digest_is_stable_lowercase_hex() {
        let d = kosync_document_digest("Piranesi", "Susanna Clarke");
        assert_eq!(d.len(), 32);
        assert_eq!(d, kosync_document_digest("Piranesi", "Susanna Clarke"));
        assert!(d.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
