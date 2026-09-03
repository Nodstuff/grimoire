//! Fractional order keys (ticket 2.2 decision: fractional index, not sibling list).
//!
//! A key is a base-36 fraction in (0, 1) written as digits, never ending in '0'.
//! `between(a, b)` returns a key strictly between its bounds; `None` means the
//! open end (start/end of the sibling list). Plain `ORDER BY order_key` sorts
//! siblings, no repair pass on concurrent inserts — losers just land adjacent.

const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE: usize = 36;

/// Why a key pair could not be bisected. `between` never surfaces this — it
/// falls back to a key after the bounds — but `try_between` does, for callers
/// that would rather reject bad input than reorder around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderKeyError {
    /// Empty, a non-base36 character, or a trailing '0' (which has no
    /// successor-free gap and would loop the bisection forever).
    Invalid(String),
    /// Lower bound is not strictly below the upper bound.
    Unordered { lower: String, upper: String },
}

impl std::fmt::Display for OrderKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(k) => write!(f, "invalid order key {k:?}: base36 digits, no trailing 0"),
            Self::Unordered { lower, upper } => {
                write!(f, "order key {lower:?} is not below {upper:?}")
            }
        }
    }
}

impl std::error::Error for OrderKeyError {}

/// A well-formed key: non-empty, lowercase base36 digits only, not ending in
/// '0' (no key can be placed between `x` and `x0`).
pub fn is_valid(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|c| DIGITS.contains(&c)) && !key.ends_with('0')
}

fn digit_val(c: u8) -> usize {
    // callers validate first (`is_valid`); an unknown byte sorts as 0 rather
    // than panicking, so a stray value can never take the store down
    DIGITS.iter().position(|&d| d == c).unwrap_or(0)
}

/// Key strictly between `a` and `b` (exclusive). `None` = open bound.
/// Total: never panics. Invalid bounds are treated as open, and `a >= b`
/// yields a key strictly after the higher bound — the caller asked for a
/// place that does not exist, so the block lands adjacent instead.
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    match try_between(a, b) {
        Ok(k) => k,
        Err(_) => {
            let a = a.filter(|k| is_valid(k));
            let b = b.filter(|k| is_valid(k));
            let lower = match (a, b) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(k), None) | (None, Some(k)) => Some(k),
                (None, None) => None,
            };
            bisect(lower.unwrap_or(""), None)
        }
    }
}

/// `between` that reports instead of falling back.
pub fn try_between(a: Option<&str>, b: Option<&str>) -> Result<String, OrderKeyError> {
    if let Some(k) = a
        && !is_valid(k)
    {
        return Err(OrderKeyError::Invalid(k.to_string()));
    }
    if let Some(k) = b
        && !is_valid(k)
    {
        return Err(OrderKeyError::Invalid(k.to_string()));
    }
    let lower = a.unwrap_or("");
    if let Some(upper) = b
        && !lower.is_empty()
        && lower >= upper
    {
        return Err(OrderKeyError::Unordered {
            lower: lower.to_string(),
            upper: upper.to_string(),
        });
    }
    Ok(bisect(lower, b))
}

/// Core bisection. Precondition (checked by the callers above): both keys
/// valid, and `a < b` when `b` is given.
fn bisect(a: &str, b: Option<&str>) -> String {
    let ab = a.as_bytes();
    let bb = b.map(str::as_bytes);
    let mut out = String::new();
    let mut i = 0;
    loop {
        let da = ab.get(i).map(|&c| digit_val(c)).unwrap_or(0);
        let db = bb.map_or(BASE, |bb| bb.get(i).map(|&c| digit_val(c)).unwrap_or(0));
        if da == db {
            out.push(DIGITS[da] as char);
            i += 1;
            continue;
        }
        if db - da > 1 {
            out.push(DIGITS[(da + db) / 2] as char);
            return out;
        }
        // db == da + 1: keep da, then bisect between rest-of-a and 1.
        out.push(DIGITS[da] as char);
        i += 1;
        loop {
            let da = ab.get(i).map(|&c| digit_val(c)).unwrap_or(0);
            if BASE - da > 1 {
                out.push(DIGITS[(da + BASE) / 2] as char);
                return out;
            }
            out.push(DIGITS[da] as char);
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OrderKeyError, between, is_valid, try_between};

    #[test]
    fn first_key_is_mid() {
        assert_eq!(between(None, None), "i");
    }

    #[test]
    fn ordering_holds() {
        let k1 = between(None, None);
        let k2 = between(Some(&k1), None);
        let k0 = between(None, Some(&k1));
        let mid = between(Some(&k0), Some(&k1));
        assert!(k0 < k1 && k1 < k2);
        assert!(k0 < mid && mid < k1);
    }

    #[test]
    fn dense_appends_and_bisections_stay_ordered() {
        // repeated append
        let mut prev = between(None, None);
        for _ in 0..100 {
            let next = between(Some(&prev), None);
            assert!(prev < next);
            prev = next;
        }
        // repeated bisection against a fixed upper bound
        let hi = between(None, None);
        let mut lo = between(None, Some(&hi));
        for _ in 0..100 {
            let mid = between(Some(&lo), Some(&hi));
            assert!(lo < mid && mid.as_str() < hi.as_str());
            lo = mid;
        }
    }

    /// Fixed vectors — shared with ui/src/editor/diff.test.ts so the UI's
    /// mirror of `between` stays bit-identical to the store's.
    #[test]
    fn fixed_vectors_shared_with_ui() {
        // shared with ui/src/editor/diff.test.ts
        let vectors: [(Option<&str>, Option<&str>, &str); 6] = [
            (None, None, "i"),
            (Some("i"), None, "r"),
            (None, Some("i"), "9"),
            (Some("i"), Some("r"), "m"),
            (Some("i"), Some("j"), "ii"),
            (Some("z"), None, "zi"),
        ];
        for (a, b, want) in vectors {
            assert_eq!(between(a, b), want, "between({a:?}, {b:?})");
        }
    }

    /// Total: equal or inverted bounds, garbage, and trailing zeros never
    /// panic or loop; the result lands strictly after the higher valid bound.
    #[test]
    fn between_is_total() {
        let k = between(Some("i"), Some("i"));
        assert!(k.as_str() > "i" && is_valid(&k));
        let k = between(Some("r"), Some("i"));
        assert!(k.as_str() > "r");
        let k = between(Some("I-1"), Some("r"));
        assert!(k.as_str() > "r", "garbage lower bound treated as open");
        let k = between(Some("i"), Some("i0"));
        assert!(k.as_str() > "i", "trailing-zero upper bound would loop; treated as open");
        assert_eq!(between(Some("!!"), None), "i");
        assert_eq!(
            try_between(Some("i"), Some("i")),
            Err(OrderKeyError::Unordered { lower: "i".into(), upper: "i".into() })
        );
        assert_eq!(try_between(None, Some("A")), Err(OrderKeyError::Invalid("A".into())));
        assert_eq!(try_between(Some("i"), Some("r")).unwrap(), "m");
    }

    #[test]
    fn validity() {
        assert!(is_valid("i"));
        assert!(is_valid("zz9"));
        assert!(!is_valid(""));
        assert!(!is_valid("i0"));
        assert!(!is_valid("I"));
        assert!(!is_valid("after:x"));
    }

    #[test]
    fn adjacent_digits() {
        // "i" and "j" are adjacent at digit 0: must recurse
        let k = between(Some("i"), Some("j"));
        assert!("i" < k.as_str() && k.as_str() < "j");
        assert!(!k.ends_with('0'));
    }
}
