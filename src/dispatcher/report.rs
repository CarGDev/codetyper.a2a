//! The compact report an A2A worker sends back, as the dispatcher states it in the task prompt. The same
//! five fields are pinned in every agent template by `docs/report-back.md`.

pub const REPORT_FIELDS: &str =
    "STATUS, SUMMARY (<=8 lines), FILES (path:line), EVIDENCE (command: result), BLOCKERS";

/// Appended to the assignee prompt: how to report and that a blocking choice is relayed complete.
pub fn assignee_tail() -> String {
    format!(" Report as {REPORT_FIELDS}; relay any blocking choice complete.")
}

/// Appended to the requester wake.
pub fn requester_tail() -> &'static str {
    " Relay any blocking choice to the user complete."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_fields_in_order_and_summary_limit() {
        let positions: Vec<usize> = ["STATUS", "SUMMARY", "FILES", "EVIDENCE", "BLOCKERS"]
            .iter()
            .map(|field| REPORT_FIELDS.find(field).expect(field))
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{REPORT_FIELDS}"
        );
        assert!(REPORT_FIELDS.contains("8 lines"), "{REPORT_FIELDS}");
    }

    #[test]
    fn tails_carry_the_fields_and_the_blocking_choice_rule() {
        let assignee = assignee_tail();
        assert!(assignee.contains(REPORT_FIELDS), "{assignee}");
        assert!(assignee.contains("blocking choice"), "{assignee}");
        assert!(assignee.len() <= 220, "{} bytes", assignee.len());
        assert!(requester_tail().contains("blocking choice"));
        assert!(!assignee.contains('\n') && !requester_tail().contains('\n'));
    }
}
