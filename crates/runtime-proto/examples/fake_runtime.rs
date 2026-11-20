//! Drive a scratch daemon's runtime channel by hand.
//!
//! ```sh
//! # a SERVER-mode scratch daemon with the channel on, and a runtime enrolled
//! HOME=$D ./target/release/taisce --db $D/ks.db --port 7513 --public-url https://scratch.example \
//!     serve --runtime-grpc-port 7514 &
//! TOKEN=$(./target/release/taisce --db $D/ks.db fabric runtime add --name fake)
//! cargo run -p taisce-runtime-proto --features client --example fake_runtime -- \
//!     --url http://127.0.0.1:7514 --token "$TOKEN"
//! ```
//!
//! `--script <file>` plays a JSON script (an array of steps, see
//! `taisce_runtime_proto::fake::Step`; the default is `demo-script.json`
//! beside this file). `--instance <id>` names the launch (default: a new id
//! each run). `--token` falls back to `TAISCE_RUNTIME_TOKEN`. Unless the
//! script ends with `"disconnect"`, the runtime stays connected, sending
//! heartbeats, until it is killed. Never point this at a real server.

use std::time::Duration;
use taisce_runtime_proto::fake::{Fake, parse_script};

const DEMO: &str = include_str!("demo-script.json");

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = arg("--url").unwrap_or_else(|| "http://127.0.0.1:7514".into());
    let token = arg("--token").or_else(|| std::env::var("TAISCE_RUNTIME_TOKEN").ok()).ok_or("--token or TAISCE_RUNTIME_TOKEN")?;
    let script = match arg("--script") {
        Some(path) => std::fs::read_to_string(path)?,
        None => DEMO.to_string(),
    };
    let steps = parse_script(&script)?;
    let instance = arg("--instance").unwrap_or_else(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        format!("fake-{t:x}")
    });
    let mut fake = Fake::connect(&url, &token, &instance, 0).await?;
    eprintln!(
        "connected as runtime {} (instance {instance}); the hub has events up to {}",
        fake.welcome.runtime_id, fake.welcome.last_up_seq
    );
    if fake.play(&steps).await? {
        eprintln!("script disconnected after event {}", fake.seq());
        return Ok(());
    }
    eprintln!("script played ({} events); staying connected — Ctrl-C to hang up", fake.seq());
    let every = Duration::from_secs(u64::from(fake.welcome.heartbeat_secs.max(1)));
    loop {
        tokio::select! {
            ended = fake.ended() => {
                match ended {
                    Some(status) => return Err(format!("the hub ended the stream: {:?}: {}", status.code(), status.message()).into()),
                    None => return Ok(()),
                }
            }
            _ = tokio::time::sleep(every) => fake.heartbeat().await?,
        }
    }
}
