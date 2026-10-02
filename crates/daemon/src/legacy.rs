//! The Grimoire → Taisce rename (0.9): the old env vars and HTTP headers
//! keep working for one release, then this file goes. Drop it in 0.10.
//!
//! Both shims translate at the edge, so nothing else in the daemon knows the
//! old names: env vars before `Cli::parse`, headers before any other layer
//! (the SERVER-mode identity pin strips `Taisce-Principal`, and that must
//! cover a client still sending `Taisce-Principal`).

use axum::http::HeaderName;

const OLD_ENV: &str = "GRIMOIRE_";
const NEW_ENV: &str = "TAISCE_";

/// Copy every `GRIMOIRE_X` into `TAISCE_X` that is not already set; returns
/// the old names it used, for `log_adopted` once logging is up. Call only
/// while the process is single-threaded (`set_var` is unsound otherwise).
pub fn adopt_env() -> Vec<String> {
    let mut used = Vec::new();
    for (k, v) in std::env::vars_os() {
        let Some(rest) = k.to_str().and_then(|k| k.strip_prefix(OLD_ENV)) else { continue };
        let new = format!("{NEW_ENV}{rest}");
        if std::env::var_os(&new).is_none() {
            // SAFETY: called first thing in `main`, before the runtime starts
            unsafe { std::env::set_var(&new, v) };
            used.push(format!("{OLD_ENV}{rest}"));
        }
    }
    used.sort();
    used
}

pub fn log_adopted(used: &[String]) {
    for old in used {
        let new = old.replacen(OLD_ENV, NEW_ENV, 1);
        tracing::warn!("{old} is deprecated (Grimoire was renamed Taisce); using it as {new} — rename it, the old name stops working in 0.10");
    }
}

/// Old header → new header. The new name wins when a request sends both.
const HEADERS: [(&str, &str); 2] = [
    ("x-grimoire-admin", crate::admin::ADMIN_HEADER),
    ("x-grimoire-principal", crate::mcp::PRINCIPAL_HEADER),
];

/// Outermost layer: rename the old headers to the new ones and drop the old
/// ones, so every later layer and handler sees only `Taisce-*`.
pub async fn rename_headers(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    rewrite(req.headers_mut());
    next.run(req).await
}

fn rewrite(headers: &mut axum::http::HeaderMap) {
    for (old, new) in HEADERS {
        let Some(v) = headers.remove(old) else { continue };
        let new = HeaderName::from_static(new);
        if !headers.contains_key(&new) {
            tracing::debug!("{old} header is deprecated; read as {new}");
            headers.insert(new, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn map(pairs: &[(&'static str, &'static str)]) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        for (k, v) in pairs {
            h.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn old_headers_are_renamed_and_removed() {
        let mut h = map(&[("x-grimoire-admin", "tok"), ("x-grimoire-principal", "claude:x")]);
        rewrite(&mut h);
        assert_eq!(h.get("taisce-admin").unwrap(), "tok");
        assert_eq!(h.get("taisce-principal").unwrap(), "claude:x");
        assert!(!h.contains_key("x-grimoire-admin") && !h.contains_key("x-grimoire-principal"));
    }

    #[test]
    fn the_new_header_wins() {
        let mut h = map(&[("x-grimoire-principal", "old"), ("taisce-principal", "new")]);
        rewrite(&mut h);
        assert_eq!(h.get("taisce-principal").unwrap(), "new");
        assert!(!h.contains_key("x-grimoire-principal"));
    }
}
