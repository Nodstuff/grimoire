//! What goes down a runtime's stream (ADR 0008 §2): signed orders, the
//! newest signed settings, pairing offers and nudges. The hub keeps each
//! until the runtime acknowledges it, and on every new stream sends again,
//! under fresh numbers, each one not yet acknowledged that is still live.
//! A row records the stream (instance) and number it last went down as, so
//! an Ack, or a reconnecting Hello's `last_down_seq`, marks it received.

use super::hub::{Hub, Refused};
use super::view::{AlertKind, Change, OrderView, SessionAlert};
use crate::store_ext::with_store;
use std::collections::HashSet;
use taisce_runtime_proto::v1 as pb;
use taisce_store::fabric::DownKind;
use taisce_store::{Scope, StoreError};
use uuid::Uuid;

/// One Down event waiting to go.
pub struct DownItem {
    /// What the stream remembers it sent: `o:<id>`, `s:<version>`, `p:<id>`, `n:<id>`.
    pub key: String,
    pub body: pb::down::Body,
    /// The row it came from (settings have none: they go by version).
    pub row: Option<(DownKind, String)>,
}

impl Hub {
    /// A stream of `instance` opened, its Hello saying it has every Down
    /// event up to `last_down_seq`: mark those received, and say what
    /// number this stream's events start after.
    pub async fn down_resume(&self, user: Uuid, runtime: Uuid, instance: &str, last_down_seq: u64, now: i64) -> Result<u64, Refused> {
        let inst = instance.to_string();
        let (got, max) = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            let got = if last_down_seq > 0 { s.fabric_down_received(runtime, &inst, last_down_seq, now)? } else { vec![] };
            Ok((got, s.fabric_down_max_seq(runtime, &inst)?))
        })
        .await
        .map_err(Refused::Store)?;
        self.publish_received(user, runtime, got).await;
        let held = {
            let fleets = self.fleets();
            fleets
                .get(&user)
                .and_then(|f| f.runtimes.get(&runtime))
                .filter(|r| r.down.0.as_deref() == Some(instance))
                .map(|r| r.down.1)
                .unwrap_or(0)
        };
        Ok(last_down_seq.max(max).max(held))
    }

    /// Everything waiting for `runtime` that this stream has not sent yet
    /// (`sent` holds the keys it has). Orders past their expiry are
    /// expired here instead, and their people told.
    pub async fn down_pending(&self, user: Uuid, runtime: Uuid, sent: &HashSet<String>, now: i64) -> Result<Vec<DownItem>, Refused> {
        let settings_version = {
            let fleets = self.fleets();
            fleets.get(&user).and_then(|f| f.runtimes.get(&runtime)).ok_or(Refused::NotLive)?.info.settings_version
        };
        let (orders, pairings, nudges, originals) = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            let nudges = s.fabric_nudges_undelivered(runtime)?;
            // a bounce may wake its receiver when what bounced could have
            let mut originals = std::collections::HashMap::new();
            for n in nudges.iter().filter(|n| n.kind == "bounce" && !n.reply_to.is_empty()) {
                if let Ok(o) = s.fabric_message(&n.reply_to) {
                    originals.insert(n.id.clone(), o.kind);
                }
            }
            Ok((s.fabric_orders_undelivered(runtime)?, s.fabric_pairings_undelivered(runtime, now)?, nudges, originals))
        })
        .await
        .map_err(Refused::Store)?;
        let mut out = vec![];
        let mut expired = vec![];
        for o in orders {
            if o.expires_at <= now {
                expired.push(o.id);
                continue;
            }
            let key = format!("o:{}", o.id);
            let (Some(payload), Some(signature)) = (o.payload, o.signature) else { continue };
            if sent.contains(&key) {
                continue;
            }
            out.push(DownItem {
                key,
                body: pb::down::Body::Order(pb::Signed { payload, device_id: o.device_id, signature }),
                row: Some((DownKind::Order, o.id)),
            });
        }
        if let Some((version, signed)) = self.settings_for(user, settings_version).await {
            let key = format!("s:{version}");
            if !sent.contains(&key) {
                out.push(DownItem { key, body: pb::down::Body::Settings(signed), row: None });
            }
        }
        for p in pairings {
            let key = format!("p:{}", p.id);
            let Some(sealed) = p.sealed else { continue };
            if !sent.contains(&key) {
                out.push(DownItem { key, body: pb::down::Body::Pairing(pb::PairingOffer { pairing_id: p.id.clone(), sealed }), row: Some((DownKind::Pairing, p.id)) });
            }
        }
        for m in nudges {
            let key = format!("n:{}", m.id);
            if sent.contains(&key) {
                continue;
            }
            let may_wake = match m.kind.as_str() {
                "ask" | "reply" => true,
                "bounce" => originals.get(&m.id).is_some_and(|k| k == "ask" || k == "reply"),
                _ => false,
            };
            let kind = match m.kind.as_str() {
                "note" => pb::MessageKind::Note,
                "ask" => pb::MessageKind::Ask,
                "reply" => pb::MessageKind::Reply,
                _ => pb::MessageKind::Bounce,
            };
            out.push(DownItem {
                key,
                body: pb::down::Body::Nudge(pb::Nudge {
                    session_id: m.to.session_id.to_string(),
                    message_id: m.id.clone(),
                    from_address: m.from.address.clone(),
                    kind: kind as i32,
                    may_wake,
                }),
                row: Some((DownKind::Nudge, m.id)),
            });
        }
        if !expired.is_empty() {
            self.expire_orders(user, runtime, expired, now).await;
        }
        Ok(out)
    }

    async fn expire_orders(&self, user: Uuid, runtime: Uuid, ids: Vec<String>, now: i64) {
        let text_off: HashSet<String> =
            self.fleets().get(&user).map(|f| f.projects.values().filter(|p| p.text_off).map(|p| p.name.clone()).collect()).unwrap_or_default();
        let gone = with_store(&self.0.store, Scope::User(user), move |s| -> Result<Vec<_>, StoreError> {
            let mut out = vec![];
            for id in ids {
                let project = s.fabric_order(runtime, &id)?.project;
                out.extend(s.fabric_order_set(runtime, &id, "expired", Some("expired"), None, Some(&["pending", "sent"]), text_off.contains(&project), now)?);
            }
            Ok(out)
        })
        .await;
        let Ok(gone) = gone else { return };
        let alerts = {
            let mut fleets = self.fleets();
            let Some(fleet) = fleets.get_mut(&user) else { return };
            let mut alerts = vec![];
            for o in &gone {
                let session = o.session_id.unwrap_or_default();
                let address = fleet.session(runtime, session).map(|s| s.address.clone()).unwrap_or_default();
                let mut a = SessionAlert::new(user, runtime, session, &address, AlertKind::OrderRefused);
                a.order_id = Some(o.id.clone());
                a.preview = Some("expired before its Mac was back".into());
                alerts.push(a);
            }
            Self::publish(fleet, gone.iter().map(|o| Change::Order(OrderView::from(o))).collect());
            fleet.finish_alerts(alerts, now)
        };
        self.alert_all(alerts);
    }

    /// A row went down as event `seq` of `instance`.
    pub async fn down_sent(&self, user: Uuid, runtime: Uuid, row: Option<(DownKind, String)>, instance: &str, seq: u64, now: i64) -> Result<(), Refused> {
        {
            let mut fleets = self.fleets();
            if let Some(rt) = fleets.get_mut(&user).and_then(|f| f.runtimes.get_mut(&runtime)) {
                rt.down = (Some(instance.to_string()), seq);
            }
        }
        let Some((kind, id)) = row else { return Ok(()) };
        let inst = instance.to_string();
        let order = with_store(&self.0.store, Scope::User(user), move |s| -> Result<_, StoreError> {
            s.fabric_down_sent(runtime, kind, &id, &inst, seq, now)?;
            Ok(if kind == DownKind::Order { Some(s.fabric_order(runtime, &id)?) } else { None })
        })
        .await
        .map_err(Refused::Store)?;
        if let Some(o) = order {
            self.publish_to(user, vec![Change::Order(OrderView::from(&o))]);
        }
        Ok(())
    }

    /// The runtime acknowledged every Down event of `instance` up to `upto`.
    pub async fn down_received(&self, user: Uuid, runtime: Uuid, instance: &str, upto: u64, now: i64) -> Result<(), Refused> {
        let inst = instance.to_string();
        let got = with_store(&self.0.store, Scope::User(user), move |s| s.fabric_down_received(runtime, &inst, upto, now)).await.map_err(Refused::Store)?;
        self.publish_received(user, runtime, got).await;
        Ok(())
    }

    async fn publish_received(&self, user: Uuid, runtime: Uuid, got: Vec<(DownKind, String)>) {
        let ids: Vec<String> = got.into_iter().filter(|(k, _)| *k == DownKind::Order).map(|(_, id)| id).collect();
        if ids.is_empty() {
            return;
        }
        let orders = with_store(&self.0.store, Scope::User(user), move |s| ids.iter().map(|id| s.fabric_order(runtime, id)).collect::<Result<Vec<_>, _>>()).await;
        if let Ok(orders) = orders {
            self.publish_to(user, orders.iter().map(|o| Change::Order(OrderView::from(o))).collect());
        }
    }
}
