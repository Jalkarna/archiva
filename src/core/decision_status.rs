use std::time::SystemTimeError;

use crate::core::dlog::DecisionRecord;
use crate::core::dmap::DecisionStatus;
use crate::core::fingerprint::{fingerprint, get_lines};
use crate::core::time::now_utc_millis;

pub fn is_fingerprint_stale(source: &str, decision: &DecisionRecord) -> bool {
    fingerprint(&get_lines(
        source,
        decision.lines_hint.start as usize,
        decision.lines_hint.end as usize,
    )) != decision.fingerprint
}

pub fn mark_stale_now(decision: &mut DecisionRecord) -> Result<(), SystemTimeError> {
    let timestamp = now_utc_millis()?;
    mark_stale(decision, timestamp);
    Ok(())
}

pub fn mark_stale(decision: &mut DecisionRecord, timestamp: impl Into<String>) {
    if decision.status != Some(DecisionStatus::Stale) {
        decision.stale_since = Some(timestamp.into());
    }
    decision.status = Some(DecisionStatus::Stale);
}

pub fn mark_orphan(decision: &mut DecisionRecord) {
    decision.status = Some(DecisionStatus::Orphan);
}

pub fn clear_recovered_status(decision: &mut DecisionRecord) -> bool {
    if matches!(
        decision.status,
        Some(DecisionStatus::Stale | DecisionStatus::Orphan)
    ) {
        decision.status = None;
        decision.stale_since = None;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{clear_recovered_status, is_fingerprint_stale, mark_orphan, mark_stale};
    use crate::core::dlog::{DecisionRecord, LineRange};
    use crate::core::dmap::DecisionStatus;
    use crate::core::fingerprint::{fingerprint, get_lines};

    #[test]
    fn detects_fingerprint_staleness_from_recorded_line_hint() {
        let source = "function kept() {\n  return 1;\n}\n";
        let decision = fixture_decision(fingerprint(&get_lines(source, 1, 3)));

        assert!(!is_fingerprint_stale(source, &decision));
        assert!(is_fingerprint_stale(
            "function kept() {\n  return 2;\n}\n",
            &decision
        ));
    }

    #[test]
    fn mark_stale_records_timestamp_only_when_transitioning_to_stale() {
        let mut decision = fixture_decision("deadbeef");

        mark_stale(&mut decision, "2026-06-26T20:31:18.340Z");
        assert_eq!(decision.status, Some(DecisionStatus::Stale));
        assert_eq!(
            decision.stale_since.as_deref(),
            Some("2026-06-26T20:31:18.340Z")
        );

        mark_stale(&mut decision, "2026-06-26T20:32:18.340Z");
        assert_eq!(
            decision.stale_since.as_deref(),
            Some("2026-06-26T20:31:18.340Z")
        );

        decision.stale_since = None;
        mark_stale(&mut decision, "2026-06-26T20:32:18.340Z");
        assert_eq!(decision.stale_since, None);

        decision.status = Some(DecisionStatus::Orphan);
        mark_stale(&mut decision, "2026-06-26T20:33:18.340Z");
        assert_eq!(
            decision.stale_since.as_deref(),
            Some("2026-06-26T20:33:18.340Z")
        );
    }

    #[test]
    fn mark_orphan_does_not_clear_existing_stale_since() {
        let mut decision = fixture_decision("deadbeef");
        decision.status = Some(DecisionStatus::Stale);
        decision.stale_since = Some("2026-06-26T20:31:18.340Z".to_string());

        mark_orphan(&mut decision);
        assert_eq!(decision.status, Some(DecisionStatus::Orphan));
        assert_eq!(
            decision.stale_since.as_deref(),
            Some("2026-06-26T20:31:18.340Z")
        );
    }

    #[test]
    fn clear_recovered_status_only_clears_stale_and_orphan() {
        let mut stale = fixture_decision("deadbeef");
        stale.status = Some(DecisionStatus::Stale);
        stale.stale_since = Some("2026-06-26T20:31:18.340Z".to_string());
        assert!(clear_recovered_status(&mut stale));
        assert_eq!(stale.status, None);
        assert_eq!(stale.stale_since, None);

        let mut orphan = fixture_decision("deadbeef");
        orphan.status = Some(DecisionStatus::Orphan);
        orphan.stale_since = Some("2026-06-26T20:31:18.340Z".to_string());
        assert!(clear_recovered_status(&mut orphan));
        assert_eq!(orphan.status, None);
        assert_eq!(orphan.stale_since, None);

        let mut undecided = fixture_decision("deadbeef");
        undecided.status = Some(DecisionStatus::Undecided);
        undecided.stale_since = Some("kept".to_string());
        assert!(!clear_recovered_status(&mut undecided));
        assert_eq!(undecided.status, Some(DecisionStatus::Undecided));
        assert_eq!(undecided.stale_since.as_deref(), Some("kept"));
    }

    fn fixture_decision(fingerprint: impl Into<String>) -> DecisionRecord {
        DecisionRecord {
            id: "dec_001".to_string(),
            lines_hint: LineRange { start: 1, end: 3 },
            fingerprint: fingerprint.into(),
            chose: "record behavior".to_string(),
            because: "fixture".to_string(),
            rejected: Vec::new(),
            expires_if: None,
            session: None,
            timestamp: "2026-06-26T20:31:18.340Z".to_string(),
            history: Vec::new(),
            status: None,
            stale_since: None,
            supersedes: None,
        }
    }
}
