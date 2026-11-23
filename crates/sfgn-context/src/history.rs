use std::fmt::Write;

/// Outcome status of a completed agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Ok,
    Failed,
}

/// Compact record of a single agent turn rendered into `=== HISTORY ===`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRecord {
    pub turn: usize,
    pub outcome: TurnOutcome,
    pub action_summary: String,
    pub result_summary: String,
    pub recovery_hint: Option<String>,
}

/// Bounded chronological turn log supporting `[OK] (DONE)` and `[FAILED + Hint]` entries.
#[derive(Debug, Clone, Default)]
pub struct TurnHistory {
    records: Vec<TurnRecord>,
}

impl TurnHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_ok(
        &mut self,
        turn: usize,
        action_summary: impl Into<String>,
        result_summary: impl Into<String>,
    ) {
        self.records.push(TurnRecord {
            turn,
            outcome: TurnOutcome::Ok,
            action_summary: action_summary.into(),
            result_summary: result_summary.into(),
            recovery_hint: None,
        });
    }

    pub fn push_failed(
        &mut self,
        turn: usize,
        action_summary: impl Into<String>,
        error_summary: impl Into<String>,
        recovery_hint: Option<String>,
    ) {
        self.records.push(TurnRecord {
            turn,
            outcome: TurnOutcome::Failed,
            action_summary: action_summary.into(),
            result_summary: error_summary.into(),
            recovery_hint,
        });
    }

    pub fn records(&self) -> &[TurnRecord] {
        &self.records
    }

    /// Renders the turn log for the user prompt.
    pub fn render(&self, max_entries: usize) -> String {
        if self.records.is_empty() {
            return "(none)\n".to_owned();
        }

        let start = self.records.len().saturating_sub(max_entries.max(4));
        let mut out = String::with_capacity(512);
        if start > 0 {
            let _ = writeln!(out, "- ... ({} earlier turns omitted)", start);
        }

        for rec in &self.records[start..] {
            match rec.outcome {
                TurnOutcome::Ok => {
                    let _ = writeln!(
                        out,
                        "- T{} [OK] (DONE): {} -> {}",
                        rec.turn, rec.action_summary, rec.result_summary
                    );
                }
                TurnOutcome::Failed => {
                    let _ = writeln!(
                        out,
                        "- T{} [FAILED]: {} -> {}",
                        rec.turn, rec.action_summary, rec.result_summary
                    );
                    if let Some(hint) = &rec.recovery_hint {
                        let _ = writeln!(out, "  Hint: {}", hint);
                    }
                }
            }
        }
        out
    }
}
