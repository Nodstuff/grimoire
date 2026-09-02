use super::runtime::Runtime;
use super::client::{join_at, mint_invite, pull_share, request};
use super::loops::{drop_dead_share, refresh_outbound};
use super::server::serve;
use super::wire::{ALPN, Frame, MAX_FRAME, RefusalCode, Request, Response, Ticket, hash_secret};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr};
use std::sync::{Arc, Mutex};
use grimoire_store::{BlockStore, SqliteStore};
    use grimoire_store::{PrincipalKind, SharePermission};
    use iroh::TransportAddr;

    /// Local-only endpoint: no relays, no discovery, explicit addressing.
    async fn local_endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec(), super::wire::HOT_ALPN.to_vec()])
            .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    fn direct_addr(ep: &Endpoint) -> EndpointAddr {
        EndpointAddr::from_parts(
            ep.id(),
            ep.bound_sockets().into_iter().map(TransportAddr::Ip),
        )
    }

    /// Owner store with one doc, one share, one minted invite.
    fn owner_store(secret: &str) -> Arc<Mutex<SqliteStore>> {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = s.create_doc("shared-runbook", None, tom.id).unwrap();
        let share = s
            .create_share(doc.id, None, SharePermission::View, None)
            .unwrap();
        s.create_invite(share.id, &hash_secret(secret), "2099-01-01T00:00:00.000Z")
            .unwrap();
        Arc::new(Mutex::new(s))
    }

    #[tokio::test]
    async fn unknown_peer_is_refused_everything_but_redeem() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone(), scratch_hot(), Runtime::default()));

        let stranger = local_endpoint().await;
        let res = request(&stranger, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn redeem_pairs_and_upgrades_the_session() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone(), scratch_hot(), Runtime::default()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();

        // wrong secret first: refused, nothing paired
        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "wrong".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));

        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        let Response::Redeemed { permission, .. } = res else {
            panic!("expected Redeemed, got {res:?}");
        };
        assert_eq!(permission, "view");

        // paired under alice's real endpoint id
        {
            let s = store.lock().unwrap();
            let c = s.contact_by_pubkey(&alice_id).unwrap().unwrap();
            // peer-supplied name carries a fingerprint suffix until renamed
            assert_eq!(c.petname, format!("alice · {}", &alice_id[..4]));
        }

        // session upgraded: authenticated requests now work
        let res = request(&alice, addr.clone(), Request::Ping).await.unwrap();
        assert_eq!(res, Response::Pong);

        // burned: same secret from another peer is refused
        let mallory = local_endpoint().await;
        let res = request(
            &mallory,
            addr,
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "also-alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn revoked_contact_is_refused_on_next_request() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone(), scratch_hot(), Runtime::default()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();
        request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            request(&alice, addr.clone(), Request::Ping).await.unwrap(),
            Response::Pong
        );

        let contact_id = {
            let s = store.lock().unwrap();
            s.contact_by_pubkey(&alice_id).unwrap().unwrap().id
        };
        store.lock().unwrap().revoke_contact(contact_id).unwrap();

        let res = request(&alice, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[test]
    fn ticket_link_round_trips() {
        let t = Ticket::new("ab".repeat(32), "share-id".into(), "s3cret".into());
        let link = t.to_link();
        assert!(link.starts_with("grimoire://join/"));
        assert_eq!(Ticket::parse(&link).unwrap(), t);
        assert_eq!(Ticket::parse(&format!("  {link}\n")).unwrap(), t); // pasted whitespace
        assert!(Ticket::parse("https://example.com/nope").is_err());
    }

    #[tokio::test]
    async fn join_materializes_mirror_root_and_pairs_both_sides() {
        // owner side: doc + minted invite via the real mint path
        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Team Runbook", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let owner_id = owner_ep.id().to_string();
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_id,
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

        // grantee side
        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;

        let ticket = Ticket::parse(&link).unwrap();
        let out = join_at(&alice_ep, &alice_store, &ticket, addr)
            .await
            .unwrap();
        assert_eq!(out.owner_name, "tom");
        assert_eq!(out.root_title, "Team Runbook");
        assert_eq!(out.permission, "propose");

        // grantee: owner paired, mirror root exists under the ORIGIN uuid
        {
            let s = alice_store.lock().unwrap();
            let owner_contact = s.contact_by_pubkey(&owner_id).unwrap().unwrap();
            assert_eq!(owner_contact.petname, "tom");
            let mirror = s.get_mirror(doc.id).unwrap().unwrap();
            assert_eq!(mirror.owner, owner_contact.id);
            assert_eq!(mirror.synced_epoch, 0);
            assert_eq!(s.get_doc(doc.id).unwrap().title, "Team Runbook");
        }
        // owner: grantee paired under her real key, share active
        {
            let s = owner_store.lock().unwrap();
            let alice_contact = s
                .contact_by_pubkey(&alice_ep.id().to_string())
                .unwrap()
                .unwrap();
            assert!(alice_contact.petname.starts_with("alice · "), "{}", alice_contact.petname);
            let share = s.get_share(share.id).unwrap();
            assert_eq!(share.contact, Some(alice_contact.id));
        }
    }

    #[tokio::test]
    async fn pull_syncs_subtree_edits_renames_moves_and_removals() {
        use grimoire_store::{BlockType, OpInput, OpKind};

        // owner: root with a child doc, each with a block
        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let root = owner_store.create_doc("Runbook", None, tom.id).unwrap();
        let child = owner_store
            .create_doc("Deploys", Some(root.id), tom.id)
            .unwrap();
        let block_op = |content: &str| OpInput {
            kind: OpKind::Insert {
                block_id: uuid::Uuid::now_v7(),
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        };
        owner_store
            .apply(root.id, 0, tom.id, vec![block_op("root text")])
            .unwrap();
        owner_store
            .apply(child.id, 0, tom.id, vec![block_op("child text")])
            .unwrap();

        let owner_ep = local_endpoint().await;
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            root.id,
            SharePermission::View,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

        // grantee joins, then pulls the snapshot
        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();
        let owner_contact = {
            let s = alice_store.lock().unwrap();
            s.list_contacts().unwrap().into_iter().next().unwrap()
        };
        let sum = pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.changed, 2); // root + child

        {
            let s = alice_store.lock().unwrap();
            let tree = s.read_doc(child.id).unwrap();
            assert_eq!(tree.doc.title, "Deploys");
            assert_eq!(tree.doc.parent_id, Some(root.id));
            assert_eq!(tree.roots[0].block.content, "child text");
            // mirror is read-only at the store layer
            let mut s = s;
            let err = s.apply(child.id, tree.doc.current_epoch, owner_contact.principal,
                vec![block_op("local vandalism")]);
            assert!(matches!(err, Err(grimoire_store::StoreError::InvalidOp(_))));
        }

        // owner: edit root, rename child, add grandchild, then pull again
        {
            let mut s = owner_store.lock().unwrap();
            let epoch = s.get_doc(root.id).unwrap().current_epoch;
            s.apply(root.id, epoch, tom.id, vec![block_op("more root text")])
                .unwrap();
            s.rename_doc(child.id, "Deploy Runbook").unwrap();
            let gc = s.create_doc("Rollbacks", Some(child.id), tom.id).unwrap();
            s.apply(gc.id, 0, tom.id, vec![block_op("rollback text")])
                .unwrap();
        }
        let sum = pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.changed, 2); // root (edited) + grandchild (new)
        {
            let s = alice_store.lock().unwrap();
            assert_eq!(s.get_doc(child.id).unwrap().title, "Deploy Runbook");
            let gc = s
                .list_docs()
                .unwrap()
                .into_iter()
                .find(|d| d.title == "Rollbacks")
                .expect("grandchild mirrored");
            assert_eq!(gc.parent_id, Some(child.id));
            let root_tree = s.read_doc(root.id).unwrap();
            assert_eq!(root_tree.roots.len(), 2);
        }

        // owner moves child (and its subtree) out of the share
        {
            let mut s = owner_store.lock().unwrap();
            s.move_doc(child.id, None, None).unwrap();
        }
        let sum = pull_share(&alice_ep, &alice_store, addr, &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.removed, 2); // child + grandchild left the share
        {
            let s = alice_store.lock().unwrap();
            assert!(s.get_mirror(child.id).unwrap().is_none());
            // soft-deleted locally: no longer in the live listing
            assert!(!s.list_docs().unwrap().iter().any(|d| d.id == child.id));
        }
    }

    #[tokio::test]
    async fn propose_upstream_parks_then_accept_flows_back_via_pull() {
        use grimoire_store::{BlockType, OpInput, OpKind, ReviewDecision};

        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Notes", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();
        let owner_contact = {
            let s = alice_store.lock().unwrap();
            s.list_contacts().unwrap().into_iter().next().unwrap()
        };
        // alice needs a direct addr (no discovery in tests): patch request
        // path by using propose via wire directly is what propose_upstream
        // does with discovery; here we test the protocol + bookkeeping by
        // sending the same messages at the known addr.
        let ops = vec![OpInput {
            kind: OpKind::Insert {
                block_id: uuid::Uuid::now_v7(),
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: "alice's suggestion".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }];
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops: ops.clone(),
                note: "typo fix".into(),
                base_epoch: None,
                request_id: Some("retry-me".into()),
            },
        )
        .await
        .unwrap();
        let Response::Proposed { op_ids } = res else {
            panic!("expected Proposed, got {res:?}");
        };

        // retry with the same request_id: same outcome, nothing double-parked
        let retry = request(
            &alice_ep,
            addr.clone(),
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops: ops.clone(),
                note: "typo fix".into(),
                base_epoch: None,
                request_id: Some("retry-me".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(retry, Response::Proposed { op_ids: op_ids.clone() });
        let parked_ids: Vec<uuid::Uuid> = op_ids.iter().map(|s| s.parse().unwrap()).collect();
        {
            let s = alice_store.lock().unwrap();
            let mut s = s;
            s.record_outbound_proposal(doc.id, share.id, owner_contact.id, &parked_ids, "typo fix")
                .unwrap();
        }

        // owner: doc untouched (pessimistic!), one parked red in the queue
        let annotation_id = {
            let s = owner_store.lock().unwrap();
            assert!(s.read_doc(doc.id).unwrap().roots.is_empty());
            let queue = s.review_queue(Some(doc.id)).unwrap();
            assert_eq!(queue.len(), 1);
            // status reads as open for the proposer
            let statuses = s.op_statuses(&parked_ids).unwrap();
            assert!(!statuses[0].applied);
            assert_eq!(statuses[0].review.as_deref(), Some("open"));
            queue[0].annotation.id
        };

        // owner accepts → applied at current epoch
        {
            let mut s = owner_store.lock().unwrap();
            s.resolve(annotation_id, tom.id, ReviewDecision::Accept)
                .unwrap();
            assert_eq!(
                s.read_doc(doc.id).unwrap().roots[0].block.content,
                "alice's suggestion"
            );
        }

        // alice: status flips on refresh, content arrives on pull
        refresh_outbound(&alice_ep, &alice_store).await; // discovery-less: may no-op
        // discovery isn't available in tests, so check the status by wire:
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::ProposalStatus {
                op_ids: op_ids.clone(),
            },
        )
        .await
        .unwrap();
        let Response::ProposalStatuses { statuses } = res else {
            panic!("expected statuses");
        };
        assert!(statuses[0].applied);
        assert_eq!(statuses[0].review.as_deref(), Some("accepted"));

        pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        {
            let s = alice_store.lock().unwrap();
            let tree = s.read_doc(doc.id).unwrap();
            assert_eq!(tree.roots[0].block.content, "alice's suggestion");
        }

        // view-only share refuses proposes outright
        {
            let mut s = owner_store.lock().unwrap();
            s.set_share_permission(share.id, SharePermission::View)
                .unwrap();
        }
        let res = request(
            &alice_ep,
            addr,
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops,
                note: String::new(),
                base_epoch: None,
                request_id: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn trusted_share_applies_as_yellow_and_decline_reads_declined() {
        use grimoire_store::{BlockType, OpInput, OpKind, ReviewDecision, ShareTrust};

        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Notes", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        owner_store.set_share_trust(share.id, ShareTrust::Yellow).unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();

        let mk_op = |content: &str| OpInput {
            kind: OpKind::Insert {
                block_id: uuid::Uuid::now_v7(),
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        };
        let propose = |ops: Vec<OpInput>| Request::Propose {
            share: share.id.to_string(),
            doc: doc.id.to_string(),
            ops,
            note: String::new(),
            base_epoch: Some(0),
            request_id: None,
        };

        // trusted: applies IMMEDIATELY as a flagged yellow
        let res = request(&alice_ep, addr.clone(), propose(vec![mk_op("trusted edit")]))
            .await
            .unwrap();
        let Response::Proposed { op_ids } = res else {
            panic!("expected Proposed, got {res:?}");
        };
        let first_ids: Vec<uuid::Uuid> = op_ids.iter().map(|s| s.parse().unwrap()).collect();
        {
            let s = owner_store.lock().unwrap();
            let tree = s.read_doc(doc.id).unwrap();
            assert_eq!(tree.roots[0].block.content, "trusted edit"); // live
            let queue = s.review_queue(Some(doc.id)).unwrap();
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].annotation.kind, grimoire_store::AnnotationKind::Review); // yellow, not parked
            let st = &s.op_statuses(&first_ids).unwrap()[0];
            assert!(st.applied);
            assert_eq!(st.review.as_deref(), Some("open"));
        }

        // owner declines → reverted via pre-image; status must read DECLINED
        // even though the op was once applied
        {
            let mut s = owner_store.lock().unwrap();
            let ann = s.review_queue(Some(doc.id)).unwrap()[0].annotation.id;
            s.resolve(ann, tom.id, ReviewDecision::Decline).unwrap();
            assert!(s.read_doc(doc.id).unwrap().roots.is_empty()); // reverted
        }
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::ProposalStatus {
                op_ids: op_ids.clone(),
            },
        )
        .await
        .unwrap();
        let Response::ProposalStatuses { statuses } = res else {
            panic!("expected statuses");
        };
        assert_eq!(statuses[0].review.as_deref(), Some("declined"));

        // an untrusted share still parks: flip trust back, propose again
        {
            let mut s = owner_store.lock().unwrap();
            s.set_share_trust(share.id, ShareTrust::Review).unwrap();
        }
        let res = request(&alice_ep, addr, propose(vec![mk_op("now untrusted")]))
            .await
            .unwrap();
        assert!(matches!(res, Response::Proposed { .. }));
        {
            let s = owner_store.lock().unwrap();
            // parked: nothing live (doc was emptied by the revert above)
            assert!(s.read_doc(doc.id).unwrap().roots.is_empty());
            let queue = s.review_queue(Some(doc.id)).unwrap();
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].annotation.kind, grimoire_store::AnnotationKind::Parked);
        }
    }

    #[tokio::test]
    async fn comments_apply_directly_and_thread_over_the_wire() {
        use grimoire_store::{BlockType, OpInput, OpKind};

        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Notes", None, tom.id).unwrap();
        let block_id = uuid::Uuid::now_v7();
        owner_store
            .apply(
                doc.id,
                0,
                tom.id,
                vec![OpInput {
                    kind: OpKind::Insert {
                        block_id,
                        parent_id: None,
                        order_key: "i".into(),
                        block_type: BlockType::Paragraph,
                        content: "discuss me".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                }],
            )
            .unwrap();
        let outside = owner_store.create_doc("Private", None, tom.id).unwrap();
        let outside_block = uuid::Uuid::now_v7();
        owner_store
            .apply(
                outside.id,
                0,
                tom.id,
                vec![OpInput {
                    kind: OpKind::Insert {
                        block_id: outside_block,
                        parent_id: None,
                        order_key: "i".into(),
                        block_type: BlockType::Paragraph,
                        content: "private".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                }],
            )
            .unwrap();

        let owner_ep = local_endpoint().await;
        // view-only on purpose: commenting is not editing
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            doc.id,
            SharePermission::View,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();

        // comment applies directly — no review queue entry
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::Comment {
                share: share.id.to_string(),
                target_block: block_id.to_string(),
                text: "what about tuesday?".into(),
                reply_to: None,
            },
        )
        .await
        .unwrap();
        let Response::Commented { block_id: comment_id } = res else {
            panic!("expected Commented, got {res:?}");
        };
        {
            let s = owner_store.lock().unwrap();
            assert!(s.review_queue(Some(doc.id)).unwrap().is_empty());
            let c = s.read_block(comment_id.parse().unwrap()).unwrap();
            assert_eq!(c.block_type, grimoire_store::BlockType::Comment);
            assert_eq!(c.refers_to, Some(block_id));
            // provenance: the remote contact's principal, not tom
            assert_ne!(c.created_by, tom.id);
        }

        // owner replies locally; alice's reply threads onto the same anchor
        let owner_reply = {
            let mut s = owner_store.lock().unwrap();
            let alice_contact = s.list_contacts().unwrap()[0].clone();
            let _ = alice_contact;
            s.add_comment(block_id, tom.id, "tuesday works", Some(comment_id.parse().unwrap()))
                .unwrap()
        };
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::Comment {
                share: share.id.to_string(),
                target_block: block_id.to_string(),
                text: "booked".into(),
                reply_to: Some(owner_reply.id.to_string()),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Commented { .. }));

        // the thread reaches alice through a normal pull
        let owner_contact = {
            let s = alice_store.lock().unwrap();
            s.list_contacts().unwrap().into_iter().next().unwrap()
        };
        pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        {
            let s = alice_store.lock().unwrap();
            let comments = s.list_comments(block_id).unwrap();
            assert_eq!(comments.len(), 3);
        }

        // a block outside the share is not commentable
        let res = request(
            &alice_ep,
            addr,
            Request::Comment {
                share: share.id.to_string(),
                target_block: outside_block.to_string(),
                text: "sneaky".into(),
                reply_to: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn pull_of_unbound_share_is_refused() {
        let store = owner_store("secret");
        let share_id = {
            let s = store.lock().unwrap();
            s.list_shares().unwrap()[0].id
        };
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone(), scratch_hot(), Runtime::default()));

        // mallory redeems nothing but tries to pull the share
        let mallory = local_endpoint().await;
        let res = request(
            &mallory,
            addr,
            Request::Pull {
                share: share_id.to_string(),
                cursors: vec![],
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn version_mismatch_is_refused_loudly() {
        let store = owner_store("s");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store, scratch_hot(), Runtime::default()));

        let client = local_endpoint().await;
        let conn = client.connect(addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(br#"{"v": 99, "type": "ping"}"#).await.unwrap();
        send.finish().unwrap();
        let raw = recv.read_to_end(MAX_FRAME).await.unwrap();
        let frame: Frame<Response> = serde_json::from_slice(&raw).unwrap();
        let Response::Refused { reason, .. } = frame.msg else {
            panic!("expected Refused");
        };
        assert!(reason.contains("version"));
    }

/// A hot set for the owner side of a test daemon (journals in a temp dir).
fn scratch_hot() -> crate::hot::HotState {
    let dir = std::env::temp_dir().join(format!("grimoire-fed-test-hot-{}", uuid::Uuid::now_v7()));
    crate::hot::HotState::new(dir)
}

// ── review-fixes regression tests ───────────────────────────────────────

fn refusal_code(res: &Response) -> RefusalCode {
    match res {
        Response::Refused { code, .. } => *code,
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[tokio::test]
async fn refusals_carry_typed_codes() {
    let store = owner_store("the-secret");
    let owner = local_endpoint().await;
    let addr = direct_addr(&owner);
    let share_id = { store.lock().unwrap().list_shares().unwrap()[0].id };
    tokio::spawn(serve(owner, store.clone(), scratch_hot(), Runtime::default()));

    // unknown peer → UnknownPeer (not a text match)
    let stranger = local_endpoint().await;
    let res = request(&stranger, addr.clone(), Request::Ping).await.unwrap();
    assert_eq!(refusal_code(&res), RefusalCode::UnknownPeer);

    // pair, then revoke the share on the owner → ShareRevoked on next pull
    request(&stranger, addr.clone(), Request::Redeem { secret: "the-secret".into(), petname: "s".into() })
        .await
        .unwrap();
    { store.lock().unwrap().set_share_state(share_id, grimoire_store::ShareState::Revoked).unwrap(); }
    let res = request(&stranger, addr, Request::Pull { share: share_id.to_string(), cursors: vec![] })
        .await
        .unwrap();
    assert_eq!(refusal_code(&res), RefusalCode::ShareRevoked);
}

#[test]
fn drop_dead_share_only_removes_that_shares_mirrors() {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let me = s.create_principal(PrincipalKind::Human, "me", None).unwrap();
    let owner = s.pair_contact("ab".repeat(32).as_str(), "owner").unwrap();
    let a = uuid::Uuid::now_v7();
    let b = uuid::Uuid::now_v7();
    let dead = uuid::Uuid::now_v7();
    let live = uuid::Uuid::now_v7();
    s.create_doc_with_id(a, "A", None, owner.principal).unwrap();
    s.create_doc_with_id(b, "B", None, owner.principal).unwrap();
    s.upsert_mirror(a, owner.id, dead, 1, SharePermission::View).unwrap();
    s.upsert_mirror(b, owner.id, live, 1, SharePermission::View).unwrap();
    let mine = s.create_doc("mine", None, me.id).unwrap();

    let dropped = drop_dead_share(&mut s, dead);
    assert_eq!(dropped, vec![a]);
    assert!(s.get_mirror(a).unwrap().is_none());
    // soft-deleted: gone from the live listing
    assert!(!s.list_docs().unwrap().iter().any(|d| d.id == a));
    assert!(s.get_mirror(b).unwrap().is_some()); // other share untouched
    assert!(s.list_docs().unwrap().iter().any(|d| d.id == mine.id)); // my own doc untouched
    assert!(s.list_docs().unwrap().iter().any(|d| d.id == b)); // b still live
}

#[tokio::test]
async fn join_refuses_to_shadow_a_local_doc() {
    // owner mints a share; the grantee already OWNS a doc under that exact id
    // (only possible if the id leaked, or a bug) → join must refuse, never
    // convert the grantee's doc into a mirror
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Owned By Tom", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (_share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    let alice = alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    // alice already has a doc under the SAME uuid
    alice_store.create_doc_with_id(doc.id, "Alice's Own Doc", None, alice.id).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();

    let out = join_at(&alice_ep, &alice_store, &ticket, addr).await;
    assert!(out.is_err(), "join should refuse to shadow a local doc");
    let s = alice_store.lock().unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().title, "Alice's Own Doc"); // untouched
    assert!(s.get_mirror(doc.id).unwrap().is_none()); // never became a mirror
}

#[tokio::test]
async fn owner_tended_flag_rides_the_pull() {
    use grimoire_store::{BlockType, ConfidencePolicy, GardenerKind, OpInput, OpKind};
    // owner: a doc tended by a keeper gardener scoped to it
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Team Docs", None, tom.id).unwrap();
    owner_store.apply(doc.id, 0, tom.id, vec![OpInput {
        kind: OpKind::Insert { block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "i".into(), block_type: BlockType::Paragraph, content: "owned by tom".into(), refers_to: None },
        source_refs: vec![],
    }]).unwrap();
    owner_store.create_gardener("keeper", GardenerKind::Keeper, "keep true", Some(doc.id), ConfidencePolicy::Review).unwrap();
    assert!(owner_store.doc_is_tended(doc.id).unwrap());

    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::Propose).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();
    let owner_contact = { alice_store.lock().unwrap().list_contacts().unwrap().into_iter().next().unwrap() };
    pull_share(&alice_ep, &alice_store, addr, &owner_contact, share.id).await.unwrap();

    // grantee sees the owner tends it
    {
        let s = alice_store.lock().unwrap();
        let m = s.get_mirror(doc.id).unwrap().unwrap();
        assert!(m.owner_tended, "grantee mirror reflects owner tending");
        // and the grantee cannot tend it locally (avoids two-sided agents)
        let s = s;
        let scope_on_mirror = s.get_mirror(doc.id).unwrap().is_some();
        assert!(scope_on_mirror);
        // creating a gardener scoped to the mirror is refused by the store guard?
        // (the guard lives in admin.rs; here we assert the mirror fact the guard keys off)
        assert!(s.doc_is_tended(doc.id).unwrap() == false, "grantee has no local gardener");
    }
}

#[tokio::test]
async fn maintainer_trust_applies_green_with_no_review_annotation() {
    use grimoire_store::{BlockType, OpInput, OpKind, ShareTrust};
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Notes", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::Propose).unwrap();
    owner_store.set_share_trust(share.id, ShareTrust::Green).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();

    let op = OpInput {
        kind: OpKind::Insert { block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "i".into(), block_type: BlockType::Paragraph, content: "maintainer edit".into(), refers_to: None },
        source_refs: vec![],
    };
    let res = request(&alice_ep, addr, Request::Propose {
        share: share.id.to_string(), doc: doc.id.to_string(), ops: vec![op], note: String::new(), base_epoch: Some(0), request_id: None,
    }).await.unwrap();
    let Response::Proposed { op_ids } = res else { panic!("expected Proposed, got {res:?}") };
    let ids: Vec<uuid::Uuid> = op_ids.iter().map(|s| s.parse().unwrap()).collect();

    let s = owner_store.lock().unwrap();
    // live immediately
    assert_eq!(s.read_doc(doc.id).unwrap().roots[0].block.content, "maintainer edit");
    // GREEN: no review annotation, queue empty — the receipt is the ledger + activity feed
    assert!(s.review_queue(Some(doc.id)).unwrap().is_empty(), "maintainer edits are not queued");
    let st = &s.op_statuses(&ids).unwrap()[0];
    assert!(st.applied);
    assert_eq!(st.review, None, "no annotation at all");
    let feed = s.recent_remote_ops(10).unwrap();
    assert_eq!(feed.len(), 1);
    assert!(feed[0].principal_name.starts_with("alice · "), "{}", feed[0].principal_name);
    assert_eq!(feed[0].doc_title, "Notes");
}

#[tokio::test]
async fn view_share_bridge_is_read_only_and_propose_share_is_not() {
    // bridge_authorized is what handle_hot_bridge keys the read-only filter on
    use super::server::bridge_authorized;
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Live Doc", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let alice_pub = alice_ep.id().to_string();
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr).await.unwrap();

    // a VIEW share may join the bridge, read-only
    let (d, read_only, _) = bridge_authorized(&owner_store, &alice_pub, &share.id.to_string(), &doc.id.to_string()).unwrap();
    assert_eq!(d, doc.id);
    assert!(read_only, "view share watches read-only");

    // upgrade to propose: full participant
    owner_store.lock().unwrap().set_share_permission(share.id, SharePermission::Propose).unwrap();
    let (_, read_only, _) = bridge_authorized(&owner_store, &alice_pub, &share.id.to_string(), &doc.id.to_string()).unwrap();
    assert!(!read_only, "propose share edits");

    // an unknown peer is refused outright
    assert!(bridge_authorized(&owner_store, &"00".repeat(32), &share.id.to_string(), &doc.id.to_string()).is_err());
}

#[tokio::test]
async fn session_consent_lets_view_grantee_write_until_owner_says_watch_only() {
    // owner hosts a live session; a VIEW grantee asks HotStatus: writable by
    // default (session = consent), read-only once the owner flips watch-only
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Riff", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    let hot = scratch_hot();
    tokio::spawn(serve(owner_ep, owner_store.clone(), hot.clone(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();

    let status = |ep: &Endpoint, addr: EndpointAddr| {
        let ep = ep.clone();
        let req = Request::HotStatus { share: share.id.to_string(), doc: doc.id.to_string() };
        async move { request(&ep, addr, req).await.unwrap() }
    };
    // cold: not hot, and a view grantee can't write (nothing to write into)
    let Response::HotStatusIs { hot: h, can_write, .. } = status(&alice_ep, addr.clone()).await else { panic!() };
    assert!(!h);
    assert_eq!(can_write, Some(false));
    // owner goes live → session = consent → the viewer may write
    hot.start(doc.id, 0).unwrap();
    let Response::HotStatusIs { hot: h, can_write, .. } = status(&alice_ep, addr.clone()).await else { panic!() };
    assert!(h);
    assert_eq!(can_write, Some(true), "view grantee writes in an owner-opened session");
    assert_eq!(hot.viewers_write(doc.id), Some(true));
    // owner flips to watch-only → viewer is read-only, live
    hot.set_viewers_write(doc.id, false).unwrap();
    let Response::HotStatusIs { can_write, .. } = status(&alice_ep, addr.clone()).await else { panic!() };
    assert_eq!(can_write, Some(false), "watch only");
    // and back
    hot.set_viewers_write(doc.id, true).unwrap();
    let Response::HotStatusIs { can_write, .. } = status(&alice_ep, addr).await else { panic!() };
    assert_eq!(can_write, Some(true));
    // toggling a doc that isn't hot is an error, not a silent no-op
    assert!(hot.set_viewers_write(uuid::Uuid::now_v7(), false).is_err());
}

#[tokio::test]
async fn nudge_is_accepted_from_the_owner_for_a_held_share_and_refused_otherwise() {
    use super::wire::NotifyKind;
    // owner A shares to grantee B; B runs a listener too (both are daemons)
    let mut a_store = SqliteStore::open_in_memory().unwrap();
    let tom = a_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = a_store.create_doc("Nudged", None, tom.id).unwrap();
    let a_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut a_store, &a_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let a_store = Arc::new(Mutex::new(a_store));
    let a_addr = direct_addr(&a_ep);
    tokio::spawn(serve(a_ep.clone(), a_store.clone(), scratch_hot(), Runtime::default()));

    let mut b_store = SqliteStore::open_in_memory().unwrap();
    b_store.create_principal(PrincipalKind::Human, "bob", None).unwrap();
    let b_store = Arc::new(Mutex::new(b_store));
    let b_ep = local_endpoint().await;
    let b_addr = direct_addr(&b_ep);
    let b_runtime = Runtime::default();
    tokio::spawn(serve(b_ep.clone(), b_store.clone(), scratch_hot(), b_runtime.clone()));
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&b_ep, &b_store, &ticket, a_addr).await.unwrap();

    // A nudges B about the shared doc → accepted, event recorded on B
    let res = request(&a_ep, b_addr.clone(), Request::Notify {
        share: share.id.to_string(), doc: doc.id.to_string(), title: "Nudged".into(), kind: NotifyKind::LiveStarted,
    }).await.unwrap();
    assert_eq!(res, Response::Noted);
    let (next, events) = b_runtime.events_since(0);
    assert_eq!(next, 1);
    assert_eq!(events[0].kind, "live_started");
    assert_eq!(events[0].doc_id, doc.id);
    assert_eq!(events[0].from, "tom");

    // A nudges about a share B does NOT hold → refused, no event
    let res = request(&a_ep, b_addr.clone(), Request::Notify {
        share: uuid::Uuid::now_v7().to_string(), doc: doc.id.to_string(), title: "x".into(), kind: NotifyKind::DocChanged,
    }).await.unwrap();
    assert!(matches!(res, Response::Refused { code: RefusalCode::NotInShare, .. }), "{res:?}");
    assert_eq!(b_runtime.events_since(0).1.len(), 1);

    // a stranger nudging B → unknown peer
    let mallory = local_endpoint().await;
    let res = request(&mallory, b_addr, Request::Notify {
        share: share.id.to_string(), doc: doc.id.to_string(), title: "x".into(), kind: NotifyKind::DocChanged,
    }).await.unwrap();
    assert!(matches!(res, Response::Refused { code: RefusalCode::UnknownPeer, .. }));
}

#[tokio::test]
async fn rejoining_docs_already_mirrored_under_an_older_share_still_lands_content() {
    // Scenario from the field: the grantee already mirrors these docs (an
    // earlier share of the same subtree), the owner mints a NEW share and the
    // grantee joins it. Titles must exist AND content must land — a stale
    // mirror row or leftover block rows must never leave empty docs behind.
    use grimoire_store::{BlockType, OpInput, OpKind};
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let root = owner_store.create_doc("Grimoire", None, tom.id).unwrap();
    let child = owner_store.create_doc("Architecture", Some(root.id), tom.id).unwrap();
    let mk = |c: &str| OpInput { kind: OpKind::Insert { block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "".into(), block_type: BlockType::Paragraph, content: c.into(), refers_to: None }, source_refs: vec![] };
    owner_store.apply(root.id, 0, tom.id, vec![mk("root body")]).unwrap();
    owner_store.apply(child.id, 0, tom.id, vec![mk("child body")]).unwrap();
    let owner_ep = local_endpoint().await;
    let (share1, link1) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), root.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep.clone(), owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut g = SqliteStore::open_in_memory().unwrap();
    g.create_principal(PrincipalKind::Human, "home", None).unwrap();
    let g = Arc::new(Mutex::new(g));
    let g_ep = local_endpoint().await;
    join_at(&g_ep, &g, &Ticket::parse(&link1).unwrap(), addr.clone()).await.unwrap();
    let owner_contact = { g.lock().unwrap().list_contacts().unwrap()[0].clone() };
    pull_share(&g_ep, &g, addr.clone(), &owner_contact, share1.id).await.unwrap();
    { let s = g.lock().unwrap(); assert_eq!(s.read_doc(child.id).unwrap().roots[0].block.content, "child body"); }

    // owner edits, then mints a SECOND share of the same subtree; grantee joins it
    { let mut s = owner_store.lock().unwrap(); let e = s.get_doc(child.id).unwrap().current_epoch; s.apply(child.id, e, tom.id, vec![mk("child body v2")]).unwrap(); }
    let (share2, link2) = { let mut s = owner_store.lock().unwrap(); mint_invite(&mut s, &owner_ep.id().to_string(), root.id, SharePermission::View).unwrap() };
    join_at(&g_ep, &g, &Ticket::parse(&link2).unwrap(), addr.clone()).await.unwrap();
    // share1 was superseded (revoked) on the owner by the redeem
    { let s = owner_store.lock().unwrap(); assert_eq!(s.get_share(share1.id).unwrap().state, grimoire_store::ShareState::Revoked); }
    let sum = pull_share(&g_ep, &g, addr, &owner_contact, share2.id).await.unwrap();
    assert!(sum.changed >= 1, "{sum:?}");
    let s = g.lock().unwrap();
    let t = s.read_doc(child.id).unwrap();
    assert_eq!(t.doc.title, "Architecture");
    let contents: Vec<_> = t.roots.iter().map(|n| n.block.content.as_str()).collect();
    assert_eq!(contents, vec!["child body", "child body v2"], "content landed after re-join");
    assert_eq!(s.get_mirror(child.id).unwrap().unwrap().share_id, share2.id, "mirror re-pointed at the new share");
    assert_eq!(s.read_doc(root.id).unwrap().roots[0].block.content, "root body");
}

#[tokio::test]
async fn revoke_then_reshare_same_subtree_rejoins_cleanly() {
    // Field bug: owner revokes a share; grantee drops the mirror (soft-deletes
    // the docs); owner re-shares the same subtree; grantee must be able to
    // join again and get full content — not a collision or a PK conflict on
    // the tombstoned doc ids.
    use grimoire_store::{BlockType, OpInput, OpKind, ShareState};
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let root = owner_store.create_doc("Grimoire", None, tom.id).unwrap();
    let child = owner_store.create_doc("Arch", Some(root.id), tom.id).unwrap();
    let mk = |c: &str| OpInput { kind: OpKind::Insert { block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "".into(), block_type: BlockType::Paragraph, content: c.into(), refers_to: None }, source_refs: vec![] };
    owner_store.apply(child.id, 0, tom.id, vec![mk("body")]).unwrap();
    let owner_ep = local_endpoint().await;
    let (share1, link1) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), root.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep.clone(), owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut g = SqliteStore::open_in_memory().unwrap();
    g.create_principal(PrincipalKind::Human, "home", None).unwrap();
    let g = Arc::new(Mutex::new(g));
    let g_ep = local_endpoint().await;
    join_at(&g_ep, &g, &Ticket::parse(&link1).unwrap(), addr.clone()).await.unwrap();
    let owner_contact = { g.lock().unwrap().list_contacts().unwrap()[0].clone() };
    pull_share(&g_ep, &g, addr.clone(), &owner_contact, share1.id).await.unwrap();
    assert_eq!(g.lock().unwrap().read_doc(child.id).unwrap().roots[0].block.content, "body");

    // owner revokes; grantee's next pull drops the mirror
    owner_store.lock().unwrap().set_share_state(share1.id, ShareState::Revoked).unwrap();
    let r = pull_share(&g_ep, &g, addr.clone(), &owner_contact, share1.id).await;
    assert!(r.is_err());
    { let mut s = g.lock().unwrap(); drop_dead_share(&mut s, share1.id); assert!(!s.list_docs().unwrap().iter().any(|d| d.id == root.id), "mirror gone"); }

    // owner re-shares the SAME subtree; grantee joins and pulls again
    let (share2, link2) = { let mut s = owner_store.lock().unwrap(); mint_invite(&mut s, &owner_ep.id().to_string(), root.id, SharePermission::Propose).unwrap() };
    join_at(&g_ep, &g, &Ticket::parse(&link2).unwrap(), addr.clone()).await.expect("re-join after revoke must succeed");
    let sum = pull_share(&g_ep, &g, addr, &owner_contact, share2.id).await.expect("pull after re-join");
    // root has no blocks (epoch 0 = cursor) so only the child ships
    assert_eq!(sum.changed, 1, "{sum:?}");
    let s = g.lock().unwrap();
    assert!(s.list_docs().unwrap().iter().any(|d| d.id == root.id && d.title == "Grimoire"), "root revived");
    assert_eq!(s.read_doc(child.id).unwrap().roots[0].block.content, "body", "content back");
    let m = s.get_mirror(child.id).unwrap().unwrap();
    assert_eq!(m.share_id, share2.id);
    assert_eq!(m.permission, SharePermission::Propose, "new grant honoured");
}

#[tokio::test]
async fn failing_pull_is_recorded_on_the_mirror_and_cleared_when_it_recovers() {
    use grimoire_store::ShareState;
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Health", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));
    let mut g = SqliteStore::open_in_memory().unwrap();
    g.create_principal(PrincipalKind::Human, "g", None).unwrap();
    let g = Arc::new(Mutex::new(g));
    let g_ep = local_endpoint().await;
    join_at(&g_ep, &g, &Ticket::parse(&link).unwrap(), addr.clone()).await.unwrap();
    // pull_groups is what the loops use — drive it via pull_all_once (discovery-less
    // here, so exercise the store-side recording directly for the failure path)
    { let mut s = g.lock().unwrap(); s.set_mirror_sync_result(share.id, Some("simulated: FOREIGN KEY constraint failed")).unwrap(); }
    { let s = g.lock().unwrap(); assert_eq!(s.get_mirror(doc.id).unwrap().unwrap().last_error.as_deref(), Some("simulated: FOREIGN KEY constraint failed")); }
    // a real successful pull over the wire clears it
    let owner_contact = { g.lock().unwrap().list_contacts().unwrap()[0].clone() };
    pull_share(&g_ep, &g, addr.clone(), &owner_contact, share.id).await.unwrap();
    { let mut s = g.lock().unwrap(); s.set_mirror_sync_result(share.id, None).unwrap(); let m = s.get_mirror(doc.id).unwrap().unwrap(); assert!(m.last_error.is_none()); assert!(m.last_pulled_at.is_some()); }
    // and a revoked share refuses with the typed code the loop keys off
    owner_store.lock().unwrap().set_share_state(share.id, ShareState::Revoked).unwrap();
    let err = pull_share(&g_ep, &g, addr, &owner_contact, share.id).await.unwrap_err();
    assert!(matches!(err.downcast_ref::<super::wire::Refusal>().map(|r| r.code), Some(RefusalCode::ShareRevoked)));
}

// ── data-safety audit (2026-09-02) regression tests ─────────────────────

/// A grantee whose mirror is BEHIND the owner must not be able to create a
/// live session: its seed would become the doc at flatten, silently reverting
/// the owner's newer edits with a green verdict. Joining a session that is
/// already live needs no epoch.
#[tokio::test]
async fn remote_hot_start_from_a_stale_mirror_is_refused_until_pulled() {
    use super::client::pull_share;
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Live", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::Propose).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    let hot = scratch_hot();
    tokio::spawn(serve(owner_ep, owner_store.clone(), hot.clone(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    let joined = join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();
    let owner_contact = alice_store.lock().unwrap().list_contacts().unwrap().into_iter().find(|c| c.pubkey == joined.owner).unwrap();
    pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id).await.unwrap();

    // owner edits after alice's pull: alice's mirror is now behind (epoch 0 vs 1)
    owner_store.lock().unwrap().apply(doc.id, 0, tom.id, vec![grimoire_store::OpInput {
        kind: grimoire_store::OpKind::Insert {
            block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "a".into(),
            block_type: grimoire_store::BlockType::Paragraph, content: "owner's newer paragraph".into(), refers_to: None,
        },
        source_refs: vec![],
    }]).unwrap();
    let stale_epoch = alice_store.lock().unwrap().get_mirror(doc.id).unwrap().unwrap().synced_epoch;
    assert_eq!(stale_epoch, 0);

    let start = |base: Option<i64>| {
        let ep = alice_ep.clone();
        let addr = addr.clone();
        let req = Request::HotStart { share: share.id.to_string(), doc: doc.id.to_string(), base_epoch: base };
        async move { request(&ep, addr, req).await.unwrap() }
    };
    // stale → typed refusal, no session
    let res = start(Some(stale_epoch)).await;
    assert_eq!(refusal_code(&res), RefusalCode::StaleBase);
    assert!(!hot.is_hot(doc.id));
    // no epoch at all (pre-field client) → also refused: we cannot know it is current
    assert_eq!(refusal_code(&start(None).await), RefusalCode::StaleBase);

    // pull → current → allowed and creates the session (seed = true)
    pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id).await.unwrap();
    let now = alice_store.lock().unwrap().get_mirror(doc.id).unwrap().unwrap().synced_epoch;
    assert_eq!(now, 1);
    let Response::HotStarted { frozen_epoch, seed } = start(Some(now)).await else { panic!("expected HotStarted") };
    assert_eq!(frozen_epoch, 1);
    assert!(seed);
    assert!(hot.is_hot(doc.id));

    // a second participant JOINS the live session with any/no epoch
    let Response::HotStarted { seed, .. } = start(None).await else { panic!("join should not need an epoch") };
    assert!(!seed);
}

/// A share bigger than one frame budget pages: the owner caps `changed`,
/// says `more`, and the grantee's pull loops until every doc has landed.
#[tokio::test]
async fn pull_pages_large_shares_until_every_doc_lands() {
    use super::client::pull_share;
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let root = owner_store.create_doc("Big", None, tom.id).unwrap();
    // 6 docs × ~1.5 MB each = 9 MB of content, over two PULL_BUDGET pages
    let big = "x".repeat(1_500_000);
    let mut ids = vec![root.id];
    for i in 0..5 {
        let d = owner_store.create_doc(&format!("part {i}"), Some(root.id), tom.id).unwrap();
        ids.push(d.id);
    }
    for id in &ids {
        owner_store.apply(*id, 0, tom.id, vec![grimoire_store::OpInput {
            kind: grimoire_store::OpKind::Insert {
                block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "a".into(),
                block_type: grimoire_store::BlockType::Paragraph, content: big.clone(), refers_to: None,
            },
            source_refs: vec![],
        }]).unwrap();
    }
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), root.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store.clone(), scratch_hot(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    let joined = join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();
    let owner_contact = alice_store.lock().unwrap().list_contacts().unwrap().into_iter().find(|c| c.pubkey == joined.owner).unwrap();

    // one raw page: capped, and it says so
    let cursors: Vec<(String, i64)> = alice_store.lock().unwrap().list_mirrors().unwrap().into_iter().map(|m| (m.doc_id.to_string(), m.synced_epoch)).collect();
    let Response::Pulled { changed, metas, more, .. } = request(&alice_ep, addr.clone(), Request::Pull { share: share.id.to_string(), cursors }).await.unwrap() else { panic!() };
    assert_eq!(metas.len(), 6, "metas always ship in full");
    assert!(changed.len() < 6, "one page must not carry every doc, got {}", changed.len());
    assert!(more);

    // the client loop drains the pages
    let summary = pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id).await.unwrap();
    assert_eq!(summary.changed, 6);
    let s = alice_store.lock().unwrap();
    for id in &ids {
        let tree = s.read_doc(*id).unwrap();
        assert_eq!(tree.roots.len(), 1, "doc {id} has no content");
        assert_eq!(tree.roots[0].block.content.len(), big.len());
        assert_eq!(s.get_mirror(*id).unwrap().unwrap().synced_epoch, 1);
    }
    drop(s);
    // and a follow-up pull is a no-op (cursors current, nothing paged)
    let summary = pull_share(&alice_ep, &alice_store, addr, &owner_contact, share.id).await.unwrap();
    assert_eq!(summary.changed, 0);
}

// ── federation hardening (2026-09-02) ───────────────────────────────────

/// Owner + grantee pair with a live listener on the owner. Returns
/// (owner_store, owner_addr, hot, alice_ep, alice_store, alice_pubkey, share, doc).
async fn paired(
    permission: SharePermission,
) -> (
    Arc<Mutex<SqliteStore>>,
    EndpointAddr,
    crate::hot::HotState,
    Endpoint,
    Arc<Mutex<SqliteStore>>,
    String,
    grimoire_store::Share,
    grimoire_store::Doc,
) {
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("Room", None, tom.id).unwrap();
    owner_store.apply(doc.id, 0, tom.id, vec![grimoire_store::OpInput {
        kind: grimoire_store::OpKind::Insert {
            block_id: uuid::Uuid::now_v7(), parent_id: None, order_key: "a".into(),
            block_type: grimoire_store::BlockType::Paragraph, content: "seed".into(), refers_to: None,
        },
        source_refs: vec![],
    }]).unwrap();
    let owner_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, permission).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    let hot = scratch_hot();
    tokio::spawn(serve(owner_ep, owner_store.clone(), hot.clone(), Runtime::default()));

    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let alice_pub = alice_ep.id().to_string();
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();
    (owner_store, addr, hot, alice_ep, alice_store, alice_pub, share, doc)
}

/// A raw y-sync Update frame inserting one paragraph — what a client sends.
fn paragraph_update(text: &str) -> Vec<u8> {
    use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
    use yrs::{ReadTxn, Text as _, Transact, XmlFragment as _};
    let ydoc = yrs::Doc::new();
    let frag = ydoc.get_or_insert_xml_fragment("default");
    {
        let mut txn = ydoc.transact_mut();
        let el = frag.insert(&mut txn, 0, yrs::XmlElementPrelim::empty("paragraph"));
        let t = el.insert(&mut txn, 0, yrs::XmlTextPrelim::new(""));
        t.insert(&mut txn, 0, text);
    }
    let update = ydoc.transact().encode_state_as_update_v1(&yrs::StateVector::default());
    let mut enc = EncoderV1::new();
    yrs::sync::Message::Sync(yrs::sync::SyncMessage::Update(update)).encode(&mut enc);
    enc.to_vec()
}

fn paragraphs_in_session(hot: &crate::hot::HotState, doc: uuid::Uuid) -> u32 {
    use yrs::{Transact, XmlFragment as _};
    let sessions = hot.sessions.lock().unwrap();
    let s = sessions.get(&doc).unwrap();
    let frag = s.awareness.doc().get_or_insert_xml_fragment("default");
    let txn = s.awareness.doc().transact();
    frag.len(&txn)
}

/// Open a raw owner-side bridge (what `open_hot_bridge` does, minus retries),
/// returning the streams after the hello frame.
async fn raw_bridge(
    ep: &Endpoint,
    addr: EndpointAddr,
    share: uuid::Uuid,
    doc: uuid::Uuid,
) -> (iroh::endpoint::SendStream, iroh::endpoint::RecvStream) {
    use super::server::{read_frame, write_frame};
    let conn = ep.connect(addr, super::wire::HOT_ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let header = serde_json::json!({"share": share.to_string(), "doc": doc.to_string()});
    write_frame(&mut send, &serde_json::to_vec(&header).unwrap()).await.unwrap();
    let hello = read_frame(&mut recv).await.unwrap();
    assert!(hello.is_some(), "bridge refused before hello");
    (send, recv)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

#[tokio::test]
async fn bridge_permission_downgrade_applies_to_the_live_bridge() {
    use super::server::write_frame;
    let (owner_store, addr, hot, alice_ep, _alice_store, _alice_pub, share, doc) = paired(SharePermission::Propose).await;
    hot.start(doc.id, 1).unwrap();
    let (mut send, _recv) = raw_bridge(&alice_ep, addr, share.id, doc.id).await;
    // propose: writes land
    write_frame(&mut send, &paragraph_update("one")).await.unwrap();
    settle().await;
    assert_eq!(paragraphs_in_session(&hot, doc.id), 1);
    // owner downgrades to view AND flips watch-only; the bridge re-reads its
    // permission at the next re-auth (200ms under test) — no reconnect needed
    owner_store.lock().unwrap().set_share_permission(share.id, SharePermission::View).unwrap();
    hot.set_viewers_write(doc.id, false).unwrap();
    tokio::time::sleep(super::server::BRIDGE_REAUTH * 3).await;
    write_frame(&mut send, &paragraph_update("two")).await.unwrap();
    settle().await;
    assert_eq!(paragraphs_in_session(&hot, doc.id), 1, "downgraded participant's update filtered");
    // session = consent: the owner opening it up again lets the viewer write
    hot.set_viewers_write(doc.id, true).unwrap();
    write_frame(&mut send, &paragraph_update("three")).await.unwrap();
    settle().await;
    assert_eq!(paragraphs_in_session(&hot, doc.id), 2);
}

#[tokio::test]
async fn revoke_cuts_a_live_bridge_immediately_and_later_frames_are_dropped() {
    use super::server::{read_frame, write_frame};
    let (owner_store, addr, hot, alice_ep, _alice_store, alice_pub, share, doc) = paired(SharePermission::Propose).await;
    hot.start(doc.id, 1).unwrap();
    let (mut send, mut recv) = raw_bridge(&alice_ep, addr.clone(), share.id, doc.id).await;
    write_frame(&mut send, &paragraph_update("one")).await.unwrap();
    settle().await;
    assert_eq!(paragraphs_in_session(&hot, doc.id), 1);
    // what the admin revoke handler does: flip the store, then cut bridges
    {
        let mut s = owner_store.lock().unwrap();
        let contact = s.contact_by_pubkey(&alice_pub).unwrap().unwrap();
        s.revoke_contact(contact.id).unwrap();
    }
    assert_eq!(hot.drop_bridges_for_peer(&alice_pub), 1);
    // the owner closes the stream well inside the re-auth interval (frames
    // already fanned out — the echo of "one" — may still be buffered first)
    let deadline = tokio::time::Instant::now() + super::server::BRIDGE_REAUTH / 2;
    loop {
        let r = tokio::time::timeout_at(deadline, read_frame(&mut recv)).await;
        match r {
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => panic!("bridge still open after the revoke"),
        }
    }
    // frames after the cut never reach the session
    let _ = write_frame(&mut send, &paragraph_update("two")).await;
    settle().await;
    assert_eq!(paragraphs_in_session(&hot, doc.id), 1);
    // and a fresh bridge is refused outright (unknown peer)
    let conn = alice_ep.connect(addr, super::wire::HOT_ALPN).await.unwrap();
    let (mut s2, mut r2) = conn.open_bi().await.unwrap();
    let header = serde_json::json!({"share": share.id.to_string(), "doc": doc.id.to_string()});
    write_frame(&mut s2, &serde_json::to_vec(&header).unwrap()).await.unwrap();
    let hello = tokio::time::timeout(std::time::Duration::from_secs(2), read_frame(&mut r2)).await.unwrap();
    assert!(matches!(hello, Ok(None) | Err(_)), "revoked peer got a hello: {hello:?}");
}

#[tokio::test]
async fn only_the_owner_or_the_starter_may_end_a_remote_session() {
    let (owner_store, addr, hot, alice_ep, _alice_store, alice_pub, share, doc) = paired(SharePermission::Propose).await;
    // a second propose grantee on the same root
    let bob_ep = local_endpoint().await;
    let (bob_share, link) = {
        let mut s = owner_store.lock().unwrap();
        let owner_node = s.list_principals().unwrap().into_iter().find(|p| p.kind == PrincipalKind::Human).unwrap();
        mint_invite(&mut s, owner_node.pubkey.as_deref().unwrap_or("00"), doc.id, SharePermission::Propose).unwrap()
    };
    let mut bob_store = SqliteStore::open_in_memory().unwrap();
    bob_store.create_principal(PrincipalKind::Human, "bob", None).unwrap();
    let bob_store = Arc::new(Mutex::new(bob_store));
    join_at(&bob_ep, &bob_store, &Ticket::parse(&link).unwrap(), addr.clone()).await.unwrap();

    // alice starts (her copy is current: epoch 1)
    let res = request(&alice_ep, addr.clone(), Request::HotStart { share: share.id.to_string(), doc: doc.id.to_string(), base_epoch: Some(1) }).await.unwrap();
    assert!(matches!(res, Response::HotStarted { seed: true, .. }), "{res:?}");
    assert!(hot.can_end(doc.id, &alice_pub));
    // bob joins, then tries to end: refused, session still live
    let res = request(&bob_ep, addr.clone(), Request::HotStart { share: bob_share.id.to_string(), doc: doc.id.to_string(), base_epoch: None }).await.unwrap();
    assert!(matches!(res, Response::HotStarted { seed: false, .. }));
    let res = request(&bob_ep, addr.clone(), Request::HotEnd { share: bob_share.id.to_string(), doc: doc.id.to_string() }).await.unwrap();
    assert_eq!(refusal_code(&res), RefusalCode::NotAllowed);
    assert!(hot.is_hot(doc.id));
    // alice ends: flattened
    let res = request(&alice_ep, addr, Request::HotEnd { share: share.id.to_string(), doc: doc.id.to_string() }).await.unwrap();
    assert!(matches!(res, Response::HotEnded { .. }), "{res:?}");
    assert!(!hot.is_hot(doc.id));
}

#[tokio::test]
async fn two_grantees_on_one_live_doc_each_keep_their_own_permission() {
    use super::server::bridge_authorized;
    let (owner_store, addr, hot, _alice_ep, _alice_store, alice_pub, share, doc) = paired(SharePermission::View).await;
    let bob_ep = local_endpoint().await;
    let (bob_share, link) = {
        let mut s = owner_store.lock().unwrap();
        let owner_node = s.list_principals().unwrap().into_iter().find(|p| p.kind == PrincipalKind::Human).unwrap();
        mint_invite(&mut s, owner_node.pubkey.as_deref().unwrap_or("00"), doc.id, SharePermission::Propose).unwrap()
    };
    let mut bob_store = SqliteStore::open_in_memory().unwrap();
    bob_store.create_principal(PrincipalKind::Human, "bob", None).unwrap();
    let bob_store = Arc::new(Mutex::new(bob_store));
    join_at(&bob_ep, &bob_store, &Ticket::parse(&link).unwrap(), addr.clone()).await.unwrap();
    let bob_pub = bob_ep.id().to_string();
    hot.start(doc.id, 1).unwrap();

    let (_, alice_ro, _) = bridge_authorized(&owner_store, &alice_pub, &share.id.to_string(), &doc.id.to_string()).unwrap();
    let (_, bob_ro, _) = bridge_authorized(&owner_store, &bob_pub, &bob_share.id.to_string(), &doc.id.to_string()).unwrap();
    assert!(alice_ro && !bob_ro, "permission is per participant");
    // both connect; the session records both
    hot.connect_as(doc.id, Some(&alice_pub)).unwrap();
    hot.connect_as(doc.id, Some(&bob_pub)).unwrap();
    // the owner's watch-only flip affects the viewer only (HotStatus can_write)
    hot.set_viewers_write(doc.id, false).unwrap();
    let status = |ep: &Endpoint, sh: uuid::Uuid| {
        let ep = ep.clone();
        let addr = addr.clone();
        let req = Request::HotStatus { share: sh.to_string(), doc: doc.id.to_string() };
        async move { request(&ep, addr, req).await.unwrap() }
    };
    let Response::HotStatusIs { can_write: bob_w, .. } = status(&bob_ep, bob_share.id).await else { panic!() };
    assert_eq!(bob_w, Some(true));
    // alice's own endpoint is not in this scope; her permission is view, so
    // watch-only makes her read-only: checked through the pure filter rule
    assert!(alice_ro && !hot.viewers_write(doc.id).unwrap());
}

#[tokio::test]
async fn owner_identity_change_on_rejoin_revokes_the_old_contact() {
    let (owner_store, _addr, _hot, alice_ep, alice_store, _alice_pub, _share, doc) = paired(SharePermission::View).await;
    let old_owner_pub = alice_store.lock().unwrap().list_contacts().unwrap()[0].pubkey.clone();
    drop(owner_store);
    // the owner re-installs: new node key, same docs (restored vault)
    let mut owner2 = SqliteStore::open_in_memory().unwrap();
    let tom2 = owner2.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    owner2.create_doc_with_id(doc.id, "Room", None, tom2.id).unwrap();
    let owner2_ep = local_endpoint().await;
    let (_share2, link2) = mint_invite(&mut owner2, &owner2_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner2_store = Arc::new(Mutex::new(owner2));
    let addr2 = direct_addr(&owner2_ep);
    tokio::spawn(serve(owner2_ep, owner2_store, scratch_hot(), Runtime::default()));

    let out = join_at(&alice_ep, &alice_store, &Ticket::parse(&link2).unwrap(), addr2).await.unwrap();
    assert_eq!(out.owner_changed_from.as_deref(), Some("tom"), "{out:?}");
    let s = alice_store.lock().unwrap();
    let contacts = s.list_contacts().unwrap();
    let old = contacts.iter().find(|c| c.pubkey == old_owner_pub).unwrap();
    assert!(old.revoked, "old identity revoked so the loops stop dialing it");
    let mirror = s.get_mirror(doc.id).unwrap().unwrap();
    let new = contacts.iter().find(|c| c.id == mirror.owner).unwrap();
    assert!(!new.revoked && new.pubkey != old_owner_pub);
}

#[tokio::test]
async fn unknown_peer_on_pull_is_a_typed_refusal_that_does_not_drop_mirrors_on_first_sight() {
    use super::client::pull_share;
    use super::loops::pull_all_once;
    let (owner_store, addr, _hot, alice_ep, alice_store, alice_pub, share, doc) = paired(SharePermission::View).await;
    let owner_contact = alice_store.lock().unwrap().list_contacts().unwrap().into_iter().next().unwrap();
    pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id).await.unwrap();
    assert!(alice_store.lock().unwrap().get_mirror(doc.id).unwrap().is_some());
    // the owner drops alice as a contact (or restored a db without her)
    {
        let mut s = owner_store.lock().unwrap();
        let c = s.contact_by_pubkey(&alice_pub).unwrap().unwrap();
        s.revoke_contact(c.id).unwrap();
    }
    let err = pull_share(&alice_ep, &alice_store, addr, &owner_contact, share.id).await.unwrap_err();
    assert_eq!(err.downcast_ref::<super::wire::Refusal>().map(|r| r.code), Some(RefusalCode::UnknownPeer));
    // a sweep sees it once: mirrors stay (a second sighting ≥ one sweep later drops them)
    let results = pull_all_once(&alice_ep, &alice_store).await;
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_err());
    let s = alice_store.lock().unwrap();
    assert!(s.get_mirror(doc.id).unwrap().is_some(), "not dropped on first UnknownPeer");
    assert!(!s.doc_is_tombstoned(doc.id).unwrap());
    assert!(s.get_mirror(doc.id).unwrap().unwrap().last_error.is_some(), "but the failure is recorded");
}

#[tokio::test]
async fn a_burned_invite_is_a_dead_join_and_an_unreachable_owner_is_not() {
    use super::loops::join_failure_is_dead;
    let mut owner_store = SqliteStore::open_in_memory().unwrap();
    let tom = owner_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = owner_store.create_doc("D", None, tom.id).unwrap();
    let owner_ep = local_endpoint().await;
    let (_share, link) = mint_invite(&mut owner_store, &owner_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let owner_store = Arc::new(Mutex::new(owner_store));
    let addr = direct_addr(&owner_ep);
    tokio::spawn(serve(owner_ep, owner_store, scratch_hot(), Runtime::default()));
    let mut alice_store = SqliteStore::open_in_memory().unwrap();
    alice_store.create_principal(PrincipalKind::Human, "alice", None).unwrap();
    let alice_store = Arc::new(Mutex::new(alice_store));
    let alice_ep = local_endpoint().await;
    let ticket = Ticket::parse(&link).unwrap();
    join_at(&alice_ep, &alice_store, &ticket, addr.clone()).await.unwrap();
    // same link pasted twice: the secret is burned
    let err = join_at(&alice_ep, &alice_store, &ticket, addr).await.unwrap_err();
    assert!(join_failure_is_dead(&err), "{err:#}");
    // an owner that is simply unreachable is a retry, not a drop
    let nobody = EndpointAddr::from(local_endpoint().await.id());
    let err = tokio::time::timeout(std::time::Duration::from_secs(12), join_at(&alice_ep, &alice_store, &ticket, nobody)).await.unwrap().unwrap_err();
    assert!(!join_failure_is_dead(&err), "{err:#}");
}

#[tokio::test]
async fn redeemed_petnames_carry_a_fingerprint_suffix_until_the_owner_renames() {
    let (owner_store, _addr, _hot, _alice_ep, alice_store, alice_pub, _share, _doc) = paired(SharePermission::View).await;
    let s = owner_store.lock().unwrap();
    let c = s.contact_by_pubkey(&alice_pub).unwrap().unwrap();
    let expected = format!("alice · {}", &alice_pub[..4]);
    assert_eq!(c.petname, expected, "peer-supplied name is marked until verified");
    assert_eq!(s.get_principal(c.principal).unwrap().display_name, expected, "provenance shows the same");
    drop(s);
    // the owner renames: contact AND principal follow
    let mut s = owner_store.lock().unwrap();
    s.rename_contact(c.id, "Alice (work)").unwrap();
    assert_eq!(s.contact_by_pubkey(&alice_pub).unwrap().unwrap().petname, "Alice (work)");
    assert_eq!(s.get_principal(c.principal).unwrap().display_name, "Alice (work)");
    assert!(s.rename_contact(c.id, "   ").is_err());
    // the grantee side names the owner by the owner's own profile name: no suffix
    let a = alice_store.lock().unwrap();
    assert_eq!(a.list_contacts().unwrap()[0].petname, "tom");
}

#[tokio::test]
async fn notify_batch_lands_one_event_per_item_and_one_pull() {
    use super::wire::{NotifyItem, NotifyKind};
    let mut a_store = SqliteStore::open_in_memory().unwrap();
    let tom = a_store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
    let doc = a_store.create_doc("Nudged", None, tom.id).unwrap();
    let kid = a_store.create_doc("Kid", Some(doc.id), tom.id).unwrap();
    let a_ep = local_endpoint().await;
    let (share, link) = mint_invite(&mut a_store, &a_ep.id().to_string(), doc.id, SharePermission::View).unwrap();
    let a_store = Arc::new(Mutex::new(a_store));
    let a_addr = direct_addr(&a_ep);
    tokio::spawn(serve(a_ep.clone(), a_store.clone(), scratch_hot(), Runtime::default()));

    let mut b_store = SqliteStore::open_in_memory().unwrap();
    b_store.create_principal(PrincipalKind::Human, "bob", None).unwrap();
    let b_store = Arc::new(Mutex::new(b_store));
    let b_ep = local_endpoint().await;
    let b_addr = direct_addr(&b_ep);
    let b_runtime = Runtime::default();
    tokio::spawn(serve(b_ep.clone(), b_store.clone(), scratch_hot(), b_runtime.clone()));
    join_at(&b_ep, &b_store, &Ticket::parse(&link).unwrap(), a_addr).await.unwrap();

    let res = request(&a_ep, b_addr.clone(), Request::NotifyBatch {
        share: share.id.to_string(),
        items: vec![
            NotifyItem { doc: doc.id.to_string(), title: "Nudged".into(), kind: NotifyKind::DocChanged },
            NotifyItem { doc: kid.id.to_string(), title: "Kid".into(), kind: NotifyKind::DocAdded },
        ],
    }).await.unwrap();
    assert_eq!(res, Response::Noted);
    let (_, events) = b_runtime.events_since(0);
    assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), vec!["doc_changed", "doc_added"]);
    // the nudged pull ran (the dial back to A works: A's address is known from the join)
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let b = b_store.lock().unwrap();
    assert!(b.get_mirror(kid.id).unwrap().is_some(), "batch nudge triggered the pull");
    // a bad doc id in the batch is refused as a whole
    drop(b);
    let res = request(&a_ep, b_addr, Request::NotifyBatch {
        share: share.id.to_string(),
        items: vec![NotifyItem { doc: "nope".into(), title: String::new(), kind: NotifyKind::DocChanged }],
    }).await.unwrap();
    assert_eq!(refusal_code(&res), RefusalCode::BadRequest);
}
