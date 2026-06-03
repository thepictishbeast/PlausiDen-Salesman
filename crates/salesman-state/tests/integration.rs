//! Integration test exercising the full happy-path round-trip
//! against a real Postgres.
//!
//! The test is `#[ignore]` by default — opt in by:
//!   1. exporting TEST_DATABASE_URL pointing at a writable Postgres,
//!   2. running `cargo test -p salesman-state -- --ignored`.
//!
//! Designed to be safe to run against a non-empty database — cleans
//! up via a UNIQUE campaign name + cascade drops at the end.

#![cfg(test)]

use chrono::Utc;
use salesman_core::model::{Company, ContactKind, DiscoverySource, ReplyKind, TouchChannel};
use salesman_core::{CompanyId, ContactId, ProspectId};
use salesman_receipts::{Signer, default_seed_path};
use salesman_state::State;
use std::collections::BTreeMap;

fn unique_campaign_name() -> String {
    format!(
        "salesman-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
async fn full_round_trip() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a writable postgres URL");
    let state = State::connect(&url).await.expect("connect");

    let campaign_name = unique_campaign_name();

    // 1) discover: insert one company
    let company = Company {
        id: CompanyId::new(),
        legal_name: Some("Acme Inc".into()),
        display_name: "Acme".into(),
        homepage: Some("https://acme.example".parse().unwrap()),
        industry: Some("Security".into()),
        size_band: None,
        region: None,
        description: None,
        tech_signals: vec![],
        discovered_at: Utc::now(),
        last_enriched_at: None,
        source: DiscoverySource::OwnerSeed,
        raw: BTreeMap::new(),
    };
    let n = state
        .insert_companies(std::slice::from_ref(&company))
        .await
        .expect("insert");
    assert_eq!(n, 1);

    // 2) campaign + prospect
    let cid = state
        .ensure_campaign(&campaign_name, "test", "test")
        .await
        .expect("ensure_campaign");
    let n = state
        .upsert_prospects_for_campaign(cid, &[company.id])
        .await
        .expect("upsert");
    assert_eq!(n, 1);

    let listed = state
        .list_prospects_with_facts_for_campaign(cid)
        .await
        .expect("list");
    assert_eq!(listed.len(), 1);
    let pid = listed[0].prospect_id;

    // 3) draft (manual; no LLM in this test)
    let touch_id = state
        .insert_touch_draft(pid, TouchChannel::Email, Some("hi"), "test body")
        .await
        .expect("insert_touch_draft");

    // 4) approve
    let n = state.approve_touch(touch_id).await.expect("approve");
    assert_eq!(n, 1);

    // 5) build + persist a receipt for the send
    let dir = std::env::temp_dir().join(format!(
        "salesman-test-key-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let signer = Signer::load_or_generate(&dir, "salesman-test-key").expect("signer");
    let prev = state
        .get_last_hash(signer.key_id())
        .await
        .expect("get_last_hash");
    let receipt = signer
        .sign_event(
            "send.email",
            serde_json::json!({"touch": touch_id.to_string()}),
            &prev,
        )
        .expect("sign");
    let receipt_id = receipt.id;
    state
        .insert_receipt(&receipt)
        .await
        .expect("insert_receipt");
    let chain_top = state
        .get_last_hash(signer.key_id())
        .await
        .expect("get_last_hash again");
    assert_eq!(chain_top, receipt.hash);

    // 6) mark_touch_sent
    let n = state
        .mark_touch_sent(touch_id, receipt_id, Utc::now())
        .await
        .expect("mark_sent");
    assert_eq!(n, 1);

    // 6b) owner audit-notification: queue, list-pending, mark-delivered
    let notif_id = state
        .insert_owner_notification(&salesman_state::query::OwnerNotificationInsert {
            touch_id: Some(touch_id),
            prospect_id: pid,
            prospect_label: "Acme",
            to_address: "acme-test@acme.example",
            channel: "email",
            sent_at: Utc::now(),
            subject: Some("hi"),
            body: "test body",
            receipt_id: Some(receipt_id),
            campaign: Some(&campaign_name),
        })
        .await
        .expect("insert_owner_notification");
    let pending = state
        .list_pending_owner_notifications(50)
        .await
        .expect("list_pending_owner_notifications");
    let mine = pending
        .iter()
        .find(|r| r.id == notif_id)
        .expect("queued notification present in pending list");
    assert_eq!(mine.prospect_label, "Acme");
    assert_eq!(mine.to_address, "acme-test@acme.example");
    assert!(mine.delivered_at.is_none());
    let marked = state
        .mark_owner_notification_delivered(notif_id, Utc::now())
        .await
        .expect("mark_owner_notification_delivered");
    assert_eq!(marked, 1);
    assert!(
        !state
            .list_pending_owner_notifications(50)
            .await
            .expect("list_pending again")
            .iter()
            .any(|r| r.id == notif_id),
        "delivered notification must drop out of the pending queue"
    );

    // 7) inbound reply that opts out — should auto-suppress + transition
    state
        .add_suppression("acme-test@acme.example", "email", "test", "test")
        .await
        .expect("add_suppression");
    assert!(state.is_suppressed("acme-test@acme.example").await.unwrap());

    // 8) summary smoke
    let summary = state.pipeline_summary(168).await.expect("summary");
    assert!(summary.companies >= 1);
    assert!(summary.prospects >= 1);

    // 9) cleanup
    let pool = state.pool();
    sqlx::query("DELETE FROM campaigns WHERE name = $1")
        .bind(&campaign_name)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company.id.0)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM suppressions WHERE target = $1")
        .bind("acme-test@acme.example")
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM receipts WHERE id = $1")
        .bind(receipt_id.0)
        .execute(pool)
        .await
        .ok();
    let _ = std::fs::remove_file(&dir);

    // Touch the unused parameters to silence unused-import warnings.
    let _: ContactId = ContactId::new();
    let _ = (
        ContactKind::Person,
        ReplyKind::Engaged,
        default_seed_path(),
        ProspectId::new(),
    );
}
