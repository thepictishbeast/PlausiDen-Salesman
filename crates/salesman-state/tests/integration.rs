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
        region: Some("Edinburgh, Scotland".into()),
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
    // region round-trips through the new ProspectWithFacts.region column.
    assert_eq!(listed[0].region.as_deref(), Some("Edinburgh, Scotland"));
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

/// Compliance-critical: an opt-out is matched against the broadened
/// candidate set (plus-addressing, Gmail dot-stripping, googlemail
/// alias, case) — and an *email* opt-out must NOT over-block the whole
/// domain. Exercises add_suppression's canonicalization + is_suppressed's
/// candidate matching end-to-end against Postgres.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
async fn suppression_broadening_round_trip() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a writable postgres URL");
    let state = State::connect(&url).await.expect("connect");

    // Unique per-run mailbox base (lowercase + digits, no dots/plus) so
    // concurrent/repeat runs don't collide.
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = format!("supptest{n}");
    let canonical = format!("{base}@gmail.com");

    // Opt out a mixed-case, plus-suffixed form; it canonicalizes on insert.
    state
        .add_suppression(
            &format!("{base}+Sales@Gmail.com"),
            "email",
            "test opt-out",
            "test",
        )
        .await
        .expect("add_suppression email");

    // The bare canonical mailbox is suppressed.
    assert!(
        state.is_suppressed(&canonical).await.unwrap(),
        "canonical gmail mailbox must be suppressed"
    );
    // A dotted + plus-suffixed googlemail.com variant of the SAME mailbox
    // is also suppressed (Gmail ignores dots; googlemail aliases gmail).
    let dotted = format!("{}.{}+promo@googlemail.com", &base[..1], &base[1..]);
    assert!(
        state.is_suppressed(&dotted).await.unwrap(),
        "dotted/plus/googlemail variant `{dotted}` must be suppressed"
    );
    // But a DIFFERENT mailbox at the same domain must NOT be suppressed —
    // an email opt-out must never over-block the whole gmail.com domain.
    assert!(
        !state
            .is_suppressed(&format!("different{n}@gmail.com"))
            .await
            .unwrap(),
        "an email opt-out must not over-block other mailboxes at the domain"
    );

    // Domain suppression blocks every address at that domain.
    let dom = format!("blocked{n}.example");
    state
        .add_suppression(&dom, "domain", "test domain block", "test")
        .await
        .expect("add_suppression domain");
    assert!(
        state.is_suppressed(&format!("anyone@{dom}")).await.unwrap(),
        "domain suppression must block any address at the domain"
    );

    // Cleanup.
    let pool = state.pool();
    sqlx::query("DELETE FROM suppressions WHERE target = $1")
        .bind(&canonical)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM suppressions WHERE target = $1")
        .bind(&dom)
        .execute(pool)
        .await
        .ok();
}

/// The consent/legal gate end-to-end: applying a classified Optout (and
/// LegalThreat) reply must transition the prospect to `suppressed`,
/// suppress the sender (canonicalized) with the correct source tag, and
/// mark the prospect's in-flight touches `suppressed` — all in one tx.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
async fn apply_reply_to_prospect_round_trip() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a writable postgres URL");
    let state = State::connect(&url).await.expect("connect");

    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let campaign_name = format!("salesman-replytest-{n}");

    // Setup: company -> campaign -> prospect -> an awaiting-approval touch.
    let company = Company {
        id: CompanyId::new(),
        legal_name: None,
        display_name: format!("ReplyTest {n}"),
        homepage: None,
        industry: None,
        size_band: None,
        region: None,
        description: None,
        tech_signals: vec![],
        discovered_at: Utc::now(),
        last_enriched_at: None,
        source: DiscoverySource::OwnerSeed,
        raw: BTreeMap::new(),
    };
    state
        .insert_companies(std::slice::from_ref(&company))
        .await
        .expect("insert company");
    let cid = state
        .ensure_campaign(&campaign_name, "test", "test")
        .await
        .expect("ensure_campaign");
    state
        .upsert_prospects_for_campaign(cid, &[company.id])
        .await
        .expect("upsert");
    let pid = state
        .list_prospects_with_facts_for_campaign(cid)
        .await
        .expect("list")[0]
        .prospect_id;
    let touch_id = state
        .insert_touch_draft(pid, TouchChannel::Email, Some("hi"), "body")
        .await
        .expect("insert_touch_draft");

    let pool = state.pool();

    // --- Optout reply ---
    let base = format!("optout{n}");
    let from = format!("{base}+promo@Gmail.com");
    let canonical = format!("{base}@gmail.com");
    let optout_reply = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO replies (id, prospect_id, from_address, body) VALUES ($1,$2,$3,$4)")
        .bind(optout_reply)
        .bind(pid.0)
        .bind(&from)
        .bind("please remove me")
        .execute(pool)
        .await
        .expect("insert optout reply");
    state
        .apply_reply_to_prospect(optout_reply, pid, &from, ReplyKind::Optout)
        .await
        .expect("apply optout");

    // prospect transitioned to suppressed
    let pstate: String = sqlx::query_scalar("SELECT state FROM prospects WHERE id = $1")
        .bind(pid.0)
        .fetch_one(pool)
        .await
        .expect("prospect state");
    assert_eq!(pstate, "suppressed", "optout must move prospect to suppressed");
    // in-flight touch suppressed
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM touches WHERE id = $1")
        .bind(touch_id.0)
        .fetch_one(pool)
        .await
        .expect("touch outcome");
    assert_eq!(outcome, "suppressed", "in-flight touch must be suppressed");
    // reply.kind persisted
    let rkind: String = sqlx::query_scalar("SELECT kind FROM replies WHERE id = $1")
        .bind(optout_reply)
        .fetch_one(pool)
        .await
        .expect("reply kind");
    assert_eq!(rkind, "optout");
    // sender suppressed by canonical form + a dotted/googlemail variant
    assert!(state.is_suppressed(&canonical).await.unwrap());
    let dotted = format!("{}.{}@googlemail.com", &base[..1], &base[1..]);
    assert!(state.is_suppressed(&dotted).await.unwrap());
    // source tag is the benign opt-out tag
    let optout_supps = state
        .list_suppressions(Some("reply_optout"), 200)
        .await
        .expect("list reply_optout");
    assert!(
        optout_supps.iter().any(|s| s.target == canonical),
        "opt-out suppression must carry source=reply_optout"
    );

    // --- LegalThreat reply (distinct source tag) ---
    let legal_from = format!("legal{n}@example.com");
    let legal_reply = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO replies (id, prospect_id, from_address, body) VALUES ($1,$2,$3,$4)")
        .bind(legal_reply)
        .bind(pid.0)
        .bind(&legal_from)
        .bind("our attorney will be in touch")
        .execute(pool)
        .await
        .expect("insert legal reply");
    state
        .apply_reply_to_prospect(legal_reply, pid, &legal_from, ReplyKind::LegalThreat)
        .await
        .expect("apply legal threat");
    assert!(state.is_suppressed(&legal_from).await.unwrap());
    let legal_supps = state
        .list_suppressions(Some("reply_legal_threat"), 200)
        .await
        .expect("list reply_legal_threat");
    assert!(
        legal_supps.iter().any(|s| s.target == legal_from),
        "legal-threat suppression must carry the DISTINCT source=reply_legal_threat"
    );

    // cleanup (campaign cascade drops prospect/touches/replies)
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
    for t in [&canonical, &legal_from] {
        sqlx::query("DELETE FROM suppressions WHERE target = $1")
            .bind(t)
            .execute(pool)
            .await
            .ok();
    }
}

/// The per-recipient rate cap (CLAUDE.md: 5 touches / 30d) must count by
/// the broadened candidate set, so it can't be bypassed by sending to
/// the same logical Gmail mailbox under dot/+suffix/googlemail aliases.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
async fn rate_cap_counts_gmail_aliases_as_one_recipient() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a writable postgres URL");
    let state = State::connect(&url).await.expect("connect");

    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let campaign_name = format!("salesman-ratetest-{n}");
    let base = format!("ratetest{n}");
    let email = format!("{base}@gmail.com");

    let company = Company {
        id: CompanyId::new(),
        legal_name: None,
        display_name: format!("RateTest {n}"),
        homepage: None,
        industry: None,
        size_band: None,
        region: None,
        description: None,
        tech_signals: vec![],
        discovered_at: Utc::now(),
        last_enriched_at: None,
        source: DiscoverySource::OwnerSeed,
        raw: BTreeMap::new(),
    };
    state
        .insert_companies(std::slice::from_ref(&company))
        .await
        .expect("insert company");
    let cid = state
        .ensure_campaign(&campaign_name, "test", "test")
        .await
        .expect("ensure_campaign");
    state
        .upsert_prospects_for_campaign(cid, &[company.id])
        .await
        .expect("upsert");
    let pid = state
        .list_prospects_with_facts_for_campaign(cid)
        .await
        .expect("list")[0]
        .prospect_id;
    state
        .insert_contact_and_link_as_primary(company.id, pid, "Jane", "Owner", &email, "test")
        .await
        .expect("link primary contact");

    // Two SENT touches to this prospect (sent_at set so they count).
    let pool = state.pool();
    for _ in 0..2 {
        let tid = state
            .insert_touch_draft(pid, TouchChannel::Email, Some("hi"), "body")
            .await
            .expect("insert_touch_draft");
        sqlx::query("UPDATE touches SET sent_at = NOW() WHERE id = $1")
            .bind(tid.0)
            .execute(pool)
            .await
            .expect("mark sent_at");
    }

    let window = 720; // 30 days
    // Canonical address: 2 sends counted.
    assert_eq!(
        state
            .count_touches_to_email_since(&email, window)
            .await
            .unwrap(),
        2
    );
    // A dotted + plus-suffixed googlemail alias of the SAME mailbox counts
    // the same — the cap is not alias-bypassable.
    let alias = format!("{}.{}+work@googlemail.com", &base[..1], &base[1..]);
    assert_eq!(
        state
            .count_touches_to_email_since(&alias, window)
            .await
            .unwrap(),
        2,
        "alias `{alias}` must count toward the same recipient's cap"
    );
    // A genuinely different mailbox at the same domain counts zero.
    assert_eq!(
        state
            .count_touches_to_email_since(&format!("{base}other@gmail.com"), window)
            .await
            .unwrap(),
        0
    );

    // cleanup (campaign cascade drops prospect/contacts/touches).
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
}

/// The per-domain rate cap (CLAUDE.md: 10 sends / hour / domain)
/// aggregates across DIFFERENT mailboxes at the domain, is
/// case-insensitive, and treats a subdomain as a separate domain.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing at a writable Postgres"]
async fn rate_cap_per_domain_aggregates_mailboxes() {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("set TEST_DATABASE_URL to a writable postgres URL");
    let state = State::connect(&url).await.expect("connect");

    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let campaign_name = format!("salesman-domaintest-{n}");
    let dom = format!("domtest{n}.example");

    // Two companies -> two prospects in one campaign, each with a
    // distinct mailbox at the SAME domain.
    let mk = |label: &str| Company {
        id: CompanyId::new(),
        legal_name: None,
        display_name: format!("DomTest {label} {n}"),
        homepage: None,
        industry: None,
        size_band: None,
        region: None,
        description: None,
        tech_signals: vec![],
        discovered_at: Utc::now(),
        last_enriched_at: None,
        source: DiscoverySource::OwnerSeed,
        raw: BTreeMap::new(),
    };
    let c1 = mk("a");
    let c2 = mk("b");
    state
        .insert_companies(&[c1.clone(), c2.clone()])
        .await
        .expect("insert companies");
    let cid = state
        .ensure_campaign(&campaign_name, "test", "test")
        .await
        .expect("ensure_campaign");
    state
        .upsert_prospects_for_campaign(cid, &[c1.id, c2.id])
        .await
        .expect("upsert");

    let pool = state.pool();
    // Map company_id -> prospect_id so we link the right contact.
    let listed = state
        .list_prospects_with_facts_for_campaign(cid)
        .await
        .expect("list");
    for (company, local) in [(&c1, "a"), (&c2, "b")] {
        let pid = listed
            .iter()
            .find(|p| p.company_id == company.id)
            .expect("prospect for company")
            .prospect_id;
        state
            .insert_contact_and_link_as_primary(
                company.id,
                pid,
                "X",
                "Owner",
                &format!("{local}@{dom}"),
                "test",
            )
            .await
            .expect("link contact");
        let tid = state
            .insert_touch_draft(pid, TouchChannel::Email, Some("hi"), "body")
            .await
            .expect("insert_touch_draft");
        sqlx::query("UPDATE touches SET sent_at = NOW() WHERE id = $1")
            .bind(tid.0)
            .execute(pool)
            .await
            .expect("mark sent_at");
    }

    let window = 24;
    // Aggregates both mailboxes at the domain.
    assert_eq!(
        state.count_touches_to_domain_since(&dom, window).await.unwrap(),
        2
    );
    // Case-insensitive on the domain.
    assert_eq!(
        state
            .count_touches_to_domain_since(&dom.to_uppercase(), window)
            .await
            .unwrap(),
        2
    );
    // A subdomain is a different domain — not counted under the parent.
    assert_eq!(
        state
            .count_touches_to_domain_since(&format!("sub.{dom}"), window)
            .await
            .unwrap(),
        0
    );
    // An unrelated domain counts zero.
    assert_eq!(
        state
            .count_touches_to_domain_since(&format!("other{n}.example"), window)
            .await
            .unwrap(),
        0
    );

    // cleanup (campaign cascade drops prospects/contacts/touches).
    sqlx::query("DELETE FROM campaigns WHERE name = $1")
        .bind(&campaign_name)
        .execute(pool)
        .await
        .ok();
    for c in [&c1, &c2] {
        sqlx::query("DELETE FROM companies WHERE id = $1")
            .bind(c.id.0)
            .execute(pool)
            .await
            .ok();
    }
}
