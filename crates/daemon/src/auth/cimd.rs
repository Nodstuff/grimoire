//! Client ID Metadata Documents: a client_id that is an https URL names a
//! JSON document the client hosts (its name and redirect_uris). The server
//! fetches it, so the fetch is an SSRF surface: https only, the host must
//! resolve to public addresses only (and the connection is pinned to the
//! addresses checked, so a second DNS answer cannot swap in 127.0.0.1), no
//! redirects, no proxy, 64 KB and 5 s caps.

use serde::Deserialize;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use webauthn_rs::prelude::Url;

pub const MAX_DOC_BYTES: usize = 64 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a fetched document is trusted before it is fetched again.
pub const CACHE_TTL: i64 = 3600;

/// What the authorize/token endpoints need from a client document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDoc {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    /// the document as fetched
    pub raw: String,
}

#[derive(Deserialize)]
struct Doc {
    client_id: String,
    #[serde(default)]
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
}

/// Is a client_id a metadata-document URL (vs a DCR-minted id)?
pub fn is_cimd(client_id: &str) -> bool {
    client_id.starts_with("https://") || client_id.starts_with("http://")
}

/// An address the server must never fetch from: loopback, private,
/// link-local (incl. cloud metadata 169.254.169.254), CGNAT, ULA,
/// unspecified, multicast, broadcast, documentation, and v4-mapped forms.
pub fn ip_is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
                || o[0] >= 240 // reserved
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ip_is_forbidden(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // ULA
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x64 && s[1] == 0xff9b) // NAT64 can reach v4 private space
                || s[..6] == [0, 0, 0, 0, 0, 0] // v4-compatible
        }
    }
}

/// Shape checks on the client_id URL before any network access.
pub fn validate_url(client_id: &str, allow_insecure: bool) -> Result<Url, String> {
    let url = Url::parse(client_id).map_err(|e| format!("client_id is not a URL: {e}"))?;
    match url.scheme() {
        "https" => {}
        "http" if allow_insecure => {}
        _ => return Err("client_id document must be https".into()),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("client_id URL must not carry credentials".into());
    }
    if url.fragment().is_some() {
        return Err("client_id URL must not have a fragment".into());
    }
    if url.path() == "/" || url.path().is_empty() {
        return Err("client_id URL must have a path".into());
    }
    if url.host_str().is_none() {
        return Err("client_id URL needs a host".into());
    }
    if client_id.len() > 2048 {
        return Err("client_id URL too long".into());
    }
    Ok(url)
}

/// Fetch and validate a client document. `allow_insecure` (tests) permits
/// http and loopback addresses; nothing else relaxes.
pub async fn fetch(client_id: &str, allow_insecure: bool) -> Result<ClientDoc, String> {
    let url = validate_url(client_id, allow_insecure)?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        _ => tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| "client_id host: DNS timeout".to_string())?
            .map_err(|e| format!("client_id host does not resolve: {e}"))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err("client_id host does not resolve".into());
    }
    if !allow_insecure && addrs.iter().any(|a| ip_is_forbidden(a.ip())) {
        return Err("client_id host resolves to a non-public address".into());
    }
    crate::install_crypto_provider();
    let mut b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(3))
        .user_agent(concat!("grimoire/", env!("CARGO_PKG_VERSION"), " (client-metadata)"));
    if matches!(url.host(), Some(url::Host::Domain(_))) {
        // pin the connection to the addresses just vetted
        b = b.resolve_to_addrs(&host, &addrs);
    }
    let client = b.build().map_err(|e| format!("http client: {e}"))?;
    let mut res = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("fetching client_id document: {e}"))?;
    if res.status() != reqwest::StatusCode::OK {
        return Err(format!("client_id document: HTTP {}", res.status().as_u16()));
    }
    if res.content_length().is_some_and(|n| n as usize > MAX_DOC_BYTES) {
        return Err("client_id document too large".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| format!("reading client_id document: {e}"))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_DOC_BYTES {
            return Err("client_id document too large".into());
        }
    }
    parse(client_id, &body)
}

/// Validate a fetched document against the URL it came from.
pub fn parse(client_id: &str, body: &[u8]) -> Result<ClientDoc, String> {
    let doc: Doc = serde_json::from_slice(body).map_err(|e| format!("client_id document is not valid JSON: {e}"))?;
    if doc.client_id != client_id {
        return Err("client_id document's client_id does not match its URL".into());
    }
    if doc.redirect_uris.is_empty() {
        return Err("client_id document has no redirect_uris".into());
    }
    if let Some(m) = &doc.token_endpoint_auth_method
        && m != "none"
    {
        return Err(format!("token_endpoint_auth_method {m:?} is not supported (public clients only)"));
    }
    if let Some(g) = &doc.grant_types
        && !g.iter().any(|g| g == "authorization_code")
    {
        return Err("client_id document does not use authorization_code".into());
    }
    if let Some(r) = &doc.response_types
        && !r.iter().any(|r| r == "code")
    {
        return Err("client_id document does not use response_type code".into());
    }
    for r in &doc.redirect_uris {
        super::oauth::check_redirect_shape(r)?;
    }
    let fallback = Url::parse(client_id).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let client_name = super::oauth::clean_name(doc.client_name.as_deref().unwrap_or(&fallback));
    Ok(ClientDoc {
        client_id: doc.client_id,
        client_name: if client_name.is_empty() { fallback } else { client_name },
        redirect_uris: doc.redirect_uris,
        raw: String::from_utf8_lossy(body).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            assert!(ip_is_forbidden(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "160.79.104.10", "2606:4700::1111"] {
            assert!(!ip_is_forbidden(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn url_shape() {
        assert!(validate_url("https://claude.ai/oauth/claude-code-client-metadata", false).is_ok());
        for bad in [
            "http://claude.ai/client.json",
            "https://claude.ai/",
            "https://claude.ai",
            "https://user:pw@claude.ai/c.json",
            "https://claude.ai/c.json#frag",
            "ftp://claude.ai/c.json",
            "not a url",
        ] {
            assert!(validate_url(bad, false).is_err(), "{bad}");
        }
        assert!(validate_url("http://127.0.0.1:9/c.json", true).is_ok());
    }

    #[tokio::test]
    async fn private_hosts_are_refused_before_any_connection() {
        for u in ["https://127.0.0.1/c.json", "https://[::1]/c.json", "https://169.254.169.254/latest/meta-data", "https://localhost/c.json"] {
            let e = fetch(u, false).await.unwrap_err();
            assert!(e.contains("non-public"), "{u}: {e}");
        }
    }

    #[test]
    fn document_validation() {
        let id = "https://app.example/client.json";
        let ok = format!(r#"{{"client_id":"{id}","client_name":"App","redirect_uris":["https://claude.ai/api/mcp/auth_callback"]}}"#);
        let d = parse(id, ok.as_bytes()).unwrap();
        assert_eq!(d.client_name, "App");
        let mismatched = r#"{"client_id":"https://evil.example/c.json","redirect_uris":["https://claude.ai/api/mcp/auth_callback"]}"#;
        assert!(parse(id, mismatched.as_bytes()).unwrap_err().contains("does not match"));
        let no_uris = format!(r#"{{"client_id":"{id}","redirect_uris":[]}}"#);
        assert!(parse(id, no_uris.as_bytes()).is_err());
        let secret = format!(
            r#"{{"client_id":"{id}","redirect_uris":["https://claude.ai/api/mcp/auth_callback"],"token_endpoint_auth_method":"client_secret_basic"}}"#
        );
        assert!(parse(id, secret.as_bytes()).is_err());
        let bad_redirect = format!(r#"{{"client_id":"{id}","redirect_uris":["https://x.example/cb#frag"]}}"#);
        assert!(parse(id, bad_redirect.as_bytes()).is_err());
        assert!(parse(id, b"not json").is_err());
        // no client_name: the host stands in
        let unnamed = format!(r#"{{"client_id":"{id}","redirect_uris":["http://127.0.0.1:3000/cb"]}}"#);
        assert_eq!(parse(id, unnamed.as_bytes()).unwrap().client_name, "app.example");
    }
}
