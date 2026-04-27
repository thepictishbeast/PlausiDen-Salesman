//! Asserts the detector behaves correctly on the curated corpus.
//!
//! Each *.txt file under tests/corpus/ has a header line:
//!   expected: high|medium|low
//! followed by `---` then the body. The detector must produce a
//! score in the matching band.

use salesman_detector::score;
use std::fs;

const HIGH_THRESHOLD: f32 = 0.7;
const MEDIUM_THRESHOLD: f32 = 0.4;

fn parse_sample(text: &str) -> (String, String) {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("").trim();
    let expected = header
        .strip_prefix("expected:")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "low".to_string());
    // Skip the --- separator line.
    let _separator = lines.next();
    let body: String = lines.collect::<Vec<_>>().join("\n");
    (expected, body)
}

#[test]
fn corpus_scores_in_expected_bands() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut failures = Vec::new();
    let mut total = 0;
    for entry in fs::read_dir(&dir).expect("corpus dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        total += 1;
        let text = fs::read_to_string(&path).expect("read");
        let (expected, body) = parse_sample(&text);
        let result = score(&body, None);
        let band = if result.score >= HIGH_THRESHOLD {
            "high"
        } else if result.score >= MEDIUM_THRESHOLD {
            "medium"
        } else {
            "low"
        };
        // We assert by category equality. "low" and "medium" can
        // mutually accept each other's middle-range — be a touch
        // tolerant on the medium boundary.
        let pass = match (expected.as_str(), band) {
            (a, b) if a == b => true,
            ("medium", "high") => true, // medium body that also shows strong tells is fine
            ("high", "medium") => false, // hard fail: a known-AI body that scores medium
            ("low", "medium") => false,  // human sample that flagged medium is a false positive
            _ => false,
        };
        if !pass {
            failures.push(format!(
                "{}: expected {}, got {} (score {:.2}; reasons: {:?})",
                path.file_name().unwrap().to_string_lossy(),
                expected,
                band,
                result.score,
                result.reasons(),
            ));
        }
    }
    assert!(total >= 6, "corpus has only {total} samples; expected >=6");
    assert!(failures.is_empty(), "corpus failures:\n  - {}", failures.join("\n  - "));
}
