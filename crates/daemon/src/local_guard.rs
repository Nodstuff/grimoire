//! Loopback-only guard against DNS rebinding.
//!
//! The daemon binds 127.0.0.1, so only local processes can reach it — but a
//! browser on this machine will happily send a request to `evil.example`
//! that an attacker has pointed at 127.0.0.1, and `/api/*` is unauthenticated.
//! Such a request always carries the attacker's hostname in `Host` (and, for
//! cross-site fetches and websocket upgrades, in `Origin`). Refusing anything
//! whose Host/Origin is not a loopback name closes the hole for every surface
//! at once: /api, /admin, /mcp, /ws/hot and the embedded UI.
//!
//! Any loopback port is accepted, not just the bound one: the Vite dev proxy
//! forwards `Host: localhost:5173` verbatim and a rebinding attacker can never
//! produce a loopback hostname at all, so the port adds nothing.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Is `host` (the value of a `Host` header, or an Origin's authority) a
/// loopback name: `127.0.0.1`, `localhost`, or `[::1]`, with an optional port?
pub fn host_is_loopback(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    let name = if let Some(rest) = host.strip_prefix('[') {
        // bracketed IPv6: `[::1]` or `[::1]:7425`
        let Some(end) = rest.find(']') else { return false };
        let tail = &rest[end + 1..];
        if !(tail.is_empty() || (tail.starts_with(':') && port_ok(&tail[1..]))) {
            return false;
        }
        return &rest[..end] == "::1";
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) => {
                if !port_ok(port) {
                    return false;
                }
                name
            }
            None => host,
        }
    };
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1"
}

fn port_ok(p: &str) -> bool {
    !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
}

/// Is `origin` (an `Origin` header) acceptable: `http://<loopback>[:port]`,
/// `ws://<loopback>[:port]`, or the shell's own scheme. `null` (opaque
/// origins, e.g. the shell's data: error page probing `/api/stamp`) is
/// refused: a rebinding attacker's page can also be sandboxed into `null`.
pub fn origin_is_local(origin: &str) -> bool {
    let origin = origin.trim();
    if origin == "tauri://localhost" || origin.starts_with("grimoire-shell://") {
        return true;
    }
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("ws://"));
    match rest {
        Some(authority) => host_is_loopback(authority.trim_end_matches('/')),
        None => false,
    }
}

/// The pure decision over a request's headers: Ok to pass, Err(reason) to
/// refuse. A missing Host is refused (HTTP/1.1 requires one; HTTP/2 puts the
/// authority in the URI, which hyper maps into Host for us). A missing Origin
/// is fine — same-origin GETs, curl and MCP clients send none.
pub fn check_headers(headers: &HeaderMap) -> Result<(), &'static str> {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    match host {
        Some(h) if host_is_loopback(h) => {}
        Some(_) => return Err("Host is not a loopback address"),
        None => return Err("missing Host header"),
    }
    if let Some(o) = headers.get(header::ORIGIN) {
        match o.to_str() {
            Ok(o) if origin_is_local(o) => {}
            _ => return Err("Origin is not a local page"),
        }
    }
    Ok(())
}

/// The axum layer: `axum::middleware::from_fn(local_guard::require_loopback)`
/// on the whole router. Websocket upgrades are ordinary GETs at this point,
/// so their Origin is checked here too.
pub async fn require_loopback(req: Request, next: Next) -> Response {
    match check_headers(req.headers()) {
        Ok(()) => next.run(req).await,
        Err(why) => {
            tracing::warn!(
                host = ?req.headers().get(header::HOST),
                origin = ?req.headers().get(header::ORIGIN),
                path = req.uri().path(),
                "refused non-loopback request: {why}"
            );
            (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from(format!(
                    "{{\"error\":\"forbidden: {why}\",\"code\":\"not_loopback\"}}"
                )),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn loopback_hosts_pass_with_or_without_port() {
        for h in [
            "127.0.0.1:7425",
            "localhost:7425",
            "LOCALHOST:7425",
            "[::1]:7425",
            "127.0.0.1",
            "localhost",
            "[::1]",
            "localhost:5173",
        ] {
            assert!(host_is_loopback(h), "{h}");
        }
    }

    #[test]
    fn rebinding_and_lookalike_hosts_are_refused() {
        for h in [
            "evil.example",
            "evil.example:7425",
            "127.0.0.1.evil.example",
            "localhost.evil.example",
            "127.0.0.2:7425",
            "0.0.0.0:7425",
            "[::2]:7425",
            "[::1]x:7425",
            "127.0.0.1:",
            "127.0.0.1:abc",
            "",
            "10.0.0.5:7425",
            "grimoire.local:7425",
        ] {
            assert!(!host_is_loopback(h), "{h}");
        }
    }

    #[test]
    fn origins() {
        assert!(origin_is_local("http://127.0.0.1:7425"));
        assert!(origin_is_local("http://localhost:7425"));
        assert!(origin_is_local("http://[::1]:7425"));
        assert!(origin_is_local("http://localhost:5173"));
        assert!(origin_is_local("tauri://localhost"));
        assert!(!origin_is_local("https://127.0.0.1:7425"));
        assert!(!origin_is_local("http://evil.example"));
        assert!(!origin_is_local("http://127.0.0.1.evil.example:7425"));
        assert!(!origin_is_local("null"));
        assert!(!origin_is_local(""));
    }

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn header_decision() {
        // what the shell, the browser UI, curl and Claude Code's MCP client send
        assert!(check_headers(&hm(&[("host", "127.0.0.1:7425")])).is_ok());
        assert!(check_headers(&hm(&[("host", "localhost:7425")])).is_ok());
        assert!(
            check_headers(&hm(&[
                ("host", "127.0.0.1:7425"),
                ("origin", "http://127.0.0.1:7425")
            ]))
            .is_ok()
        );
        // rebinding: attacker hostname in Host, or a foreign Origin on a
        // websocket/cross-site fetch
        assert!(check_headers(&hm(&[("host", "evil.example:7425")])).is_err());
        assert!(
            check_headers(&hm(&[
                ("host", "127.0.0.1:7425"),
                ("origin", "http://evil.example")
            ]))
            .is_err()
        );
        assert!(check_headers(&hm(&[])).is_err());
    }

    #[tokio::test]
    async fn layer_returns_403_and_passes_loopback() {
        use axum::body::Body;
        use axum::http::Request;
        use axum::routing::get;
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route("/x", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(require_loopback));
        let res = app
            .clone()
            .oneshot(
                Request::get("/x")
                    .header("host", "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let res = app
            .oneshot(
                Request::get("/x")
                    .header("host", "localhost:7425")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
