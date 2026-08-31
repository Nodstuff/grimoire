//! Fractional order keys (ticket 2.2 decision: fractional index, not sibling list).
//!
//! A key is a base-36 fraction in (0, 1) written as digits, never ending in '0'.
//! `between(a, b)` returns a key strictly between its bounds; `None` means the
//! open end (start/end of the sibling list). Plain `ORDER BY order_key` sorts
//! siblings, no repair pass on concurrent inserts — losers just land adjacent.

const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE: usize = 36;

fn digit_val(c: u8) -> usize {
    DIGITS
        .iter()
        .position(|&d| d == c)
        .expect("invalid order-key digit")
}

/// Key strictly between `a` and `b` (exclusive). `None` = open bound.
/// Precondition: `a < b` lexicographically when both are given; keys never end in '0'.
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("");
    if let (Some(bs), false) = (b, a.is_empty()) {
        assert!(
            a < bs,
            "order_key: lower bound {a:?} not below upper bound {bs:?}"
        );
    }
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
    use super::between;

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

    #[test]
    fn adjacent_digits() {
        // "i" and "j" are adjacent at digit 0: must recurse
        let k = between(Some("i"), Some("j"));
        assert!("i" < k.as_str() && k.as_str() < "j");
        assert!(!k.ends_with('0'));
    }
}
