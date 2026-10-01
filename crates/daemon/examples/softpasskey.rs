//! Dev tool for live smoke tests of SERVER mode: plays the browser with
//! webauthn-rs's software passkey. Registers a passkey through an
//! enrollment link, then signs in on an authorize URL and prints the
//! redirect (carrying the code). Never part of the shipped binary.
//!
//!   cargo run --example softpasskey -- <enroll-url> <authorize-url>

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
    let ccr: CreationChallengeResponse = serde_json::from_value(b["options"].clone())?;
    let cred = pk.do_registration(origin.clone(), ccr).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let f: Value = post("/auth/enroll/finish", json!({"ceremony": b["ceremony"], "credential": cred})).await?.json().await?;
    eprintln!("enroll: {f}");

    let page = http.get(authorize).send().await?.text().await?;
    let req = regex::Regex::new(r#"data-req="([^"]+)""#)?
        .captures(&page)
        .map(|c| c[1].to_string())
        .ok_or_else(|| anyhow::anyhow!("no sign-in request on the authorize page:\n{page}"))?;
    let rcr: RequestChallengeResponse = post("/oauth/authorize/begin", json!({"req": req})).await?.json().await?;
    let a = pk.do_authentication(origin, rcr).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let r: Value = post("/oauth/authorize/finish", json!({"req": req, "credential": a})).await?.json().await?;
    println!("{}", r["redirect"].as_str().unwrap_or(&r.to_string()));
    Ok(())
}
