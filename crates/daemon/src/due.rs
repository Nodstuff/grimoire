//! Plain-text deadlines for the To-do list: a TRAILING `due <when>` / `by
//! <when>` phrase on an item's line (case-insensitive; a leading `·`, `—`, `–`
//! or `-` separator is swallowed too). Nothing mid-sentence is ever touched —
//! `read the due diligence doc` and `send it by email` have no deadline.
//!
//! `<when>` is one of:
//!
//! | phrase                          | meaning                                        |
//! |---------------------------------|------------------------------------------------|
//! | `today`, `tomorrow`             |                                                |
//! | `mon` … `sunday`                | the next such weekday; TODAY counts            |
//! | `next <weekday>`                | the one after that (the next-rule date + 7)    |
//! | `in N days` / `in N weeks`      | (`day`/`week` singular accepted)               |
//! | `YYYY-MM-DD`                    | ISO — the stored form                          |
//! | `D/M`, `D/M/YYYY`, `D/M/YY`     | DAY/MONTH (European); no year → roll forward   |
//! | `D mon`, `mon D`, `D september` | English month names / 3-letter forms, optional |
//! |                                 | year after; no year → roll forward             |
//!
//! Numeric dates are DAY/MONTH — `12/9` is 12 September — because the owner is
//! European. That is a deliberate fixed choice, not a setting (yet); a
//! month/day reading would silently make `12/9` December. A date with no year
//! that has already passed this year means next year (`31/12` typed on 31 Dec
//! is today; `1/1` typed then is tomorrow).
//!
//! A trailing phrase that starts like a date (a digit, `next`, `in`, a weekday
//! or month name) but does not parse — `due 31/2`, `due nextt fri` — is left
//! exactly as typed with a warning the API surfaces; a phrase that does not
//! look like a date at all (`due diligence`) is plain text.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

pub const WARNING: &str = "couldn't read that date";

/// The stored form: `text · due 2026-09-12`.
pub const SEP: &str = "·";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// the item text with the phrase removed (unchanged when nothing parsed)
    pub text: String,
    pub deadline: Option<NaiveDate>,
    /// set when a trailing date-like phrase could not be read
    pub warning: Option<&'static str>,
}

/// Longest `<when>` in tokens (`12 sep 2027`, `in 3 days`, `next fri`).
const MAX_WHEN: usize = 3;

/// Pull a trailing `due <when>` / `by <when>` phrase off `raw`.
pub fn split_due(raw: &str, today: NaiveDate) -> Parsed {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    let untouched = || Parsed { text: tokens.join(" "), deadline: None, warning: None };
    // the keyword must be within the last MAX_WHEN+1 tokens, with ≥1 token after it
    let lo = tokens.len().saturating_sub(MAX_WHEN + 1);
    let Some(kw) = (lo..tokens.len().saturating_sub(1))
        .rev()
        .find(|&i| matches!(tokens[i].to_ascii_lowercase().as_str(), "due" | "by"))
    else {
        return untouched();
    };
    let when: Vec<String> = tokens[kw + 1..].iter().map(|t| t.to_ascii_lowercase()).collect();
    if !looks_like_a_date(&when) {
        return untouched();
    }
    let mut head = &tokens[..kw];
    if let Some(last) = head.last()
        && matches!(*last, "·" | "—" | "–" | "-")
    {
        head = &head[..head.len() - 1];
    }
    match parse_when(&when, today) {
        Some(d) => Parsed { text: head.join(" "), deadline: Some(d), warning: None },
        None => Parsed { text: tokens.join(" "), deadline: None, warning: Some(WARNING) },
    }
}

/// Any token a digit, `today`/`tomorrow`/`next`/`in`, a weekday or a month.
fn looks_like_a_date(when: &[String]) -> bool {
    when.iter().map(String::as_str).any(|t| {
        t.chars().any(|c| c.is_ascii_digit())
            || matches!(t, "today" | "tomorrow" | "next" | "in")
            || weekday(t).is_some()
            || month(t).is_some()
    })
}

fn parse_when(when: &[String], today: NaiveDate) -> Option<NaiveDate> {
    let w: Vec<&str> = when.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["today"] => Some(today),
        ["tomorrow"] => Some(today + Duration::days(1)),
        [d] if weekday(d).is_some() => Some(next_weekday(today, weekday(d)?, false)),
        ["next", d] => Some(next_weekday(today, weekday(d)?, true)),
        ["in", n, unit] => {
            let n: i64 = n.parse().ok().filter(|n| (1..=3650).contains(n))?;
            match *unit {
                "day" | "days" => Some(today + Duration::days(n)),
                "week" | "weeks" => Some(today + Duration::weeks(n)),
                _ => None,
            }
        }
        [one] => iso(one).or_else(|| slashed(one, today)),
        [a, b] => day_month(a, b, None, today),
        [a, b, y] => day_month(a, b, Some(y), today),
        _ => None,
    }
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month(s: &str) -> Option<u32> {
    const NAMES: [&str; 12] = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november",
        "december",
    ];
    if s.len() < 3 {
        return None;
    }
    NAMES
        .iter()
        .position(|m| *m == s || (s.len() == 3 && m.starts_with(s)) || (s == "sept" && *m == "september"))
        .map(|i| i as u32 + 1)
}

/// The next `wd` on or after today (`next`: the one a week later).
fn next_weekday(today: NaiveDate, wd: Weekday, next: bool) -> NaiveDate {
    let ahead = (wd.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
    today + Duration::days(ahead + if next { 7 } else { 0 })
}

fn iso(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// `12`, `12th`, `1st` → 12, 1
fn day_num(s: &str) -> Option<u32> {
    let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &s[digits.len()..];
    if !matches!(suffix, "" | "st" | "nd" | "rd" | "th") {
        return None;
    }
    digits.parse().ok().filter(|d| (1..=31).contains(d))
}

fn year_num(s: &str) -> Option<i32> {
    match s.len() {
        4 => s.parse().ok(),
        2 => s.parse::<i32>().ok().map(|y| 2000 + y),
        _ => None,
    }
}

/// `D/M`, `D/M/YYYY`, `D/M/YY` — DAY first (see the module doc).
fn slashed(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [d, m] => with_year(d.parse().ok()?, m.parse().ok()?, None, today),
        [d, m, y] => with_year(d.parse().ok()?, m.parse().ok()?, Some(year_num(y)?), today),
        _ => None,
    }
}

/// `12 sep` / `sep 12` (+ optional year)
fn day_month(a: &str, b: &str, y: Option<&str>, today: NaiveDate) -> Option<NaiveDate> {
    let (d, m) = match (day_num(a), month(b)) {
        (Some(d), Some(m)) => (d, m),
        _ => (day_num(b)?, month(a)?),
    };
    let y = match y {
        Some(y) => Some(year_num(y)?),
        None => None,
    };
    with_year(d, m, y, today)
}

/// No year: this year, or next when that date is already behind us.
fn with_year(d: u32, m: u32, y: Option<i32>, today: NaiveDate) -> Option<NaiveDate> {
    if let Some(y) = y {
        return NaiveDate::from_ymd_opt(y, m, d);
    }
    match NaiveDate::from_ymd_opt(today.year(), m, d) {
        Some(x) if x >= today => Some(x),
        // past this year (or a 29 Feb this year does not exist): next year
        _ => NaiveDate::from_ymd_opt(today.year() + 1, m, d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Friday 11 September 2026
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 11).unwrap()
    }

    fn p(s: &str) -> Parsed {
        split_due(s, today())
    }

    fn due(s: &str) -> (String, String) {
        let r = p(s);
        assert!(r.warning.is_none(), "{s}: {r:?}");
        (r.text, r.deadline.map(|d| d.to_string()).unwrap_or_default())
    }

    fn day(s: &str) -> String {
        due(&format!("x due {s}")).1
    }

    #[test]
    fn relative_words_and_weekdays() {
        assert_eq!(day("today"), "2026-09-11");
        assert_eq!(day("Tomorrow"), "2026-09-12");
        assert_eq!(day("fri"), "2026-09-11", "today is Friday and counts");
        assert_eq!(day("friday"), "2026-09-11");
        assert_eq!(day("next fri"), "2026-09-18", "next = the one after today's");
        assert_eq!(day("sat"), "2026-09-12");
        assert_eq!(day("mon"), "2026-09-14");
        assert_eq!(day("Monday"), "2026-09-14");
        assert_eq!(day("next monday"), "2026-09-21");
        assert_eq!(day("tues"), "2026-09-15");
        assert_eq!(day("thurs"), "2026-09-17");
        assert_eq!(day("sun"), "2026-09-13");
        assert_eq!(day("in 3 days"), "2026-09-14");
        assert_eq!(day("in 1 day"), "2026-09-12");
        assert_eq!(day("in 2 weeks"), "2026-09-25");
        assert_eq!(day("in 1 week"), "2026-09-18");
    }

    #[test]
    fn numeric_dates_are_day_month_and_roll_forward() {
        assert_eq!(day("2026-09-12"), "2026-09-12");
        assert_eq!(day("12/9"), "2026-09-12");
        assert_eq!(day("12/09"), "2026-09-12");
        assert_eq!(day("1/1"), "2027-01-01", "already past this year → next year");
        assert_eq!(day("11/9"), "2026-09-11", "today itself is not past");
        assert_eq!(day("10/9"), "2027-09-10");
        assert_eq!(day("12/9/2027"), "2027-09-12");
        assert_eq!(day("12/09/2027"), "2027-09-12");
        assert_eq!(day("12/9/27"), "2027-09-12");
        assert_eq!(day("1/1/2026"), "2026-01-01", "an explicit year is taken as is");
        // year end: 31 Dec today → 1/1 is tomorrow
        let nye = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(split_due("x due 1/1", nye).deadline.unwrap().to_string(), "2027-01-01");
        assert_eq!(split_due("x due 31/12", nye).deadline.unwrap().to_string(), "2026-12-31");
        assert_eq!(split_due("x due 2 jan", nye).deadline.unwrap().to_string(), "2027-01-02");
        // 29 Feb: not this year, so next (2028 is a leap year)
        let d = NaiveDate::from_ymd_opt(2027, 3, 1).unwrap();
        assert_eq!(split_due("x due 29/2", d).deadline.unwrap().to_string(), "2028-02-29");
    }

    #[test]
    fn month_names_both_orders() {
        assert_eq!(day("12 sep"), "2026-09-12");
        assert_eq!(day("sep 12"), "2026-09-12");
        assert_eq!(day("12 september"), "2026-09-12");
        assert_eq!(day("September 12"), "2026-09-12");
        assert_eq!(day("12 Sept"), "2026-09-12");
        assert_eq!(day("12th sep"), "2026-09-12");
        assert_eq!(day("1st jan"), "2027-01-01");
        assert_eq!(day("12 sep 2027"), "2027-09-12");
        assert_eq!(day("sep 12 2027"), "2027-09-12");
        assert_eq!(day("3 aug"), "2027-08-03", "rolls forward");
    }

    #[test]
    fn by_and_separators_and_the_stored_form() {
        assert_eq!(due("call the bank by fri"), ("call the bank".into(), "2026-09-11".into()));
        assert_eq!(due("call the bank BY 12/9"), ("call the bank".into(), "2026-09-12".into()));
        assert_eq!(due("ship it · due 2026-09-12"), ("ship it".into(), "2026-09-12".into()));
        assert_eq!(due("ship it — due tomorrow"), ("ship it".into(), "2026-09-12".into()));
        assert_eq!(due("ship it - due mon"), ("ship it".into(), "2026-09-14".into()));
        assert_eq!(due("  ship   it   Due  Mon  "), ("ship it".into(), "2026-09-14".into()));
        assert_eq!(due("due tomorrow"), ("".into(), "2026-09-12".into()), "a bare phrase leaves no text");
    }

    #[test]
    fn mid_sentence_words_are_never_touched() {
        for s in [
            "read the due diligence doc",
            "send the report by email",
            "sort out payments by direct debit",
            "due",
            "stand by",
            "what is it due to",
            "fix the bug due to the crash",
        ] {
            let r = p(s);
            assert_eq!(r, Parsed { text: s.to_string(), deadline: None, warning: None }, "{s}");
        }
        // the phrase must be at the END
        let r = p("due fri call the bank");
        assert_eq!(r.text, "due fri call the bank");
        assert!(r.deadline.is_none() && r.warning.is_none());
    }

    #[test]
    fn a_date_like_phrase_that_does_not_parse_warns_and_keeps_the_text() {
        for s in ["pay rent due 31/2", "pay rent due 32 sep", "pay rent due 12/13", "x due in 3 fortnights", "x due nextt fri", "x due in x days", "x due 2026-02-30", "x by 0/1"] {
            let r = p(s);
            assert_eq!(r.text, s, "{s}");
            assert!(r.deadline.is_none(), "{s}");
            assert_eq!(r.warning, Some(WARNING), "{s}");
        }
    }
}
