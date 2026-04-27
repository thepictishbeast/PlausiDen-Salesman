//! Asserts every draft fixture under tests/draft_fixtures/ passes
//! the detector ensemble (score < APPROVAL_THRESHOLD).
//!
//! These fixtures represent the QUALITY BAR for any draft we'd
//! actually approve and send. If a heuristic in the detector
//! changes and starts flagging good drafts, this test catches it
//! BEFORE the change ships.
//!
//! Each *.txt file has the format:
//!   subject: <subject line>
//!   ---
//!   <body>
//!
//! BUG ASSUMPTION: the detector ensemble is calibrated such that
//! human-quality / well-edited drafts score well below 0.6. If we
//! lower the threshold (or add a heuristic that catches a
//! legitimate pattern), this test will fail loudly.

use salesman_detector::score;
use std::fs;

/// Same threshold as the operator-facing `salesman score` default
/// (which the preflight gate also uses). A draft scoring at or
/// above this is considered AI-detector-blockable.
const APPROVAL_THRESHOLD: f32 = 0.6;

fn parse_fixture(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("");
    let subject = header.strip_prefix("subject:").map(|s| s.trim().to_string());
    let _separator = lines.next();
    let body = lines.collect::<Vec<_>>().join("\n");
    (subject, body)
}

#[test]
fn every_draft_fixture_passes_detector() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/draft_fixtures");
    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("read draft_fixtures dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().and_then(|x| x.to_str()) == Some("txt")
        })
        .collect();

    assert!(
        entries.len() >= 4,
        "expected at least 4 draft fixtures, got {}",
        entries.len()
    );

    let mut failures = Vec::new();
    for entry in &entries {
        let path = entry.path();
        let text = fs::read_to_string(&path).expect("read fixture");
        let (subject, body) = parse_fixture(&text);
        let result = score(&body, subject.as_deref());
        if result.score >= APPROVAL_THRESHOLD {
            failures.push(format!(
                "{}: scored {:.3} (>= {:.2}); reasons: {:?}",
                path.file_name().unwrap().to_string_lossy(),
                result.score,
                APPROVAL_THRESHOLD,
                result.reasons(),
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "draft-fixture detector failures (these should be approvable but flagged):\n  - {}",
        failures.join("\n  - ")
    );
}

#[test]
fn fixtures_have_subject_and_body() {
    // Sanity guard against malformed fixtures that would silently
    // pass `every_draft_fixture_passes_detector` because their body
    // ended up empty.
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/draft_fixtures");
    for entry in fs::read_dir(&dir).expect("read") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|x| x.to_str()) != Some("txt") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read");
        let (subject, body) = parse_fixture(&text);
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            subject.is_some() && !subject.as_ref().unwrap().is_empty(),
            "{name}: missing or empty `subject:` header",
        );
        assert!(
            body.trim().chars().count() >= 50,
            "{name}: body is shorter than 50 chars — likely malformed fixture",
        );
    }
}
