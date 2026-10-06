//! Dev tool for live smoke tests of SERVER mode: plays the browser with
//! webauthn-rs's software passkey. Registers a passkey through an
//! enrollment link, then signs in on an authorize URL and prints the
//! redirect (carrying the code). Never part of the shipped binary.
//!
//!   cargo run --example softpasskey -- <enroll-url> <authorize-url>
//!
//! The server enrolls discoverable passkeys (`residentKey: required`) and
//! signs in with an empty `allowCredentials` (ADR 0004). webauthn-rs's
//! SoftPasskey is U2F underneath: it can neither store a resident key nor
//! find one, so this tool plays that part the way `auth::tests` does: it
//! drops the legacy `requireResidentKey` flag after checking enrollment asks
//! for a discoverable credential, and at sign-in names the credential it
//! just made, as a platform authenticator finding its own passkey would.
//! The signed clientData is the server's challenge, unchanged.

use serde_json::{Value, json};
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [enroll, authorize] = &args[..] else {
        anyhow::bail!("usage: softpasskey <enroll-url> <authorize-url>");
    };
    let enroll = Url::parse(enroll)?;
    let origin = Url::parse(&enroll.origin().ascii_serialization())?;
    let base = origin.as_str().trim_end_matches('/').to_string();
    let http = reqwest::Client::new();
    let post = |path: &str, body: Value| {
        http.post(format!("{base}{path}")).header("origin", &base).json(&body).send()
    };
    let mut pk = WebauthnAuthenticator::new(SoftPasskey::new(true));

    let t = enroll.query_pairs().find(|(k, _)| k == "t").map(|(_, v)| v.into_owned()).unwrap_or_default();
    let b: Value = post("/auth/enroll/begin", json!({"t": t, "label": "softpasskey"})).await?.json().await?;
    let mut options = b["options"].clone();
    let sel = options
        .pointer_mut("/publicKey/authenticatorSelection")
        .ok_or_else(|| anyhow::anyhow!("enrollment options without authenticatorSelection"))?;
    anyhow::ensure!(sel["residentKey"] == "required", "the server no longer asks for a discoverable passkey: {sel}");
    sel["requireResidentKey"] = json!(false);
    let ccr: CreationChallengeResponse = serde_json::from_value(options)?;
    let cred = pk.do_registration(origin.clone(), ccr).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let cred_id = serde_json::to_value(&cred)?["id"].as_str().unwrap_or_default().to_string();
    let f: Value = post("/auth/enroll/finish", json!({"ceremony": b["ceremony"], "credential": cred})).await?.json().await?;
    eprintln!("enroll: {f}");

    let page = http.get(authorize).send().await?.text().await?;
    let req = regex::Regex::new(r#"data-req="([^"]+)""#)?
        .captures(&page)
        .map(|c| c[1].to_string())
        .ok_or_else(|| anyhow::anyhow!("no sign-in request on the authorize page:\n{page}"))?;
    let mut options: Value = post("/oauth/authorize/begin", json!({"req": req})).await?.json().await?;
    anyhow::ensure!(options["publicKey"]["allowCredentials"] == json!([]), "sign-in names a credential: {options}");
    options["publicKey"]["allowCredentials"] = json!([{"type": "public-key", "id": cred_id}]);
    let rcr: RequestChallengeResponse = serde_json::from_value(options)?;
    let a = pk.do_authentication(origin, rcr).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let r: Value = post("/oauth/authorize/finish", json!({"req": req, "credential": a})).await?.json().await?;
    println!("{}", r["redirect"].as_str().unwrap_or(&r.to_string()));
    Ok(())
}
