use chrono::{DateTime, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvStatus {
    Active,
    Waiting,
    Idle,
}
// Note: ConvStatus is independent of ThreadStatus — it tracks conversation-level
// activity for peripheral awareness only, not thread lifecycle.

#[derive(Debug, Clone)]
pub struct ConversationSummary {
    pub principal_id: String,
    pub channel: String,
    pub summary: String,
    pub status: ConvStatus,
    pub last_active: DateTime<Utc>,
}

pub struct SituationalAwareness {
    pub entries: std::collections::HashMap<String, ConversationSummary>,
    recency_hours: u64,
}

impl SituationalAwareness {
    pub fn new(recency_hours: u64) -> Self {
        Self { entries: Default::default(), recency_hours }
    }

    pub fn set_active(&mut self, principal_id: &str, channel: &str, summary: &str) {
        let entry = self.entries.entry(principal_id.to_string()).or_insert_with(|| ConversationSummary {
            principal_id: principal_id.to_string(),
            channel: channel.to_string(),
            summary: summary.to_string(),
            status: ConvStatus::Active,
            last_active: Utc::now(),
        });
        entry.status = ConvStatus::Active;
        entry.channel = channel.to_string();
        entry.summary = summary.to_string();
        entry.last_active = Utc::now();
    }

    pub fn set_idle(&mut self, principal_id: &str) {
        if let Some(entry) = self.entries.get_mut(principal_id) {
            entry.status = ConvStatus::Idle;
            entry.last_active = Utc::now();
        }
    }

    pub fn set_waiting(&mut self, principal_id: &str) {
        if let Some(entry) = self.entries.get_mut(principal_id) {
            entry.status = ConvStatus::Waiting;
            entry.last_active = Utc::now();
        }
    }

    /// Generate the peripheral awareness block for injection into the system prompt.
    /// Only includes entries active within the recency window.
    /// The `current_principal` entry is labeled "(current focus)".
    pub fn render(&self, current_principal: &str, max_tokens_approx: usize) -> String {
        let cutoff = Utc::now() - chrono::Duration::hours(self.recency_hours as i64);
        let mut lines: Vec<String> = self.entries.values()
            .filter(|e| e.last_active >= cutoff)
            .map(|e| {
                let status_label = if e.principal_id == current_principal {
                    "current focus".to_string()
                } else {
                    match e.status {
                        ConvStatus::Active => "active".to_string(),
                        ConvStatus::Waiting => "awaiting response".to_string(),
                        ConvStatus::Idle => "idle".to_string(),
                    }
                };
                let age = Utc::now().signed_duration_since(e.last_active);
                let age_str = if age.num_minutes() < 1 { "just now".to_string() }
                    else if age.num_hours() < 1 { format!("{}m ago", age.num_minutes()) }
                    else { format!("{}h ago", age.num_hours()) };
                format!("• {} [{}] — {} — {} ({})", e.principal_id, e.channel, e.summary, status_label, age_str)
            })
            .collect();
        lines.sort(); // deterministic ordering

        // Approximate token budget: ~4 chars/token
        let budget_chars = max_tokens_approx * 4;
        let mut result = String::from("## Active Conversations\n");
        for line in &lines {
            if result.len() + line.len() + 1 > budget_chars {
                result.push_str(&format!("• ({} more, within {}h)\n", lines.len() - result.lines().count() + 1, self.recency_hours));
                break;
            }
            result.push_str(line);
            result.push('\n');
        }
        result
    }

    pub fn active_count(&self) -> usize {
        let cutoff = Utc::now() - chrono::Duration::hours(self.recency_hours as i64);
        self.entries.values().filter(|e| e.last_active >= cutoff).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_active_entries() {
        let mut sa = SituationalAwareness::new(24);
        sa.set_active("jared", "telegram", "discussing identity design");
        sa.set_active("claude-code", "nats", "memory protection task");
        sa.set_waiting("claude-code");
        let output = sa.render("jared", 500);
        assert!(output.contains("jared"));
        assert!(output.contains("current focus"));
        assert!(output.contains("claude-code"));
        assert!(output.contains("awaiting response"));
    }

    #[test]
    fn render_excludes_old_entries() {
        let mut sa = SituationalAwareness::new(1); // 1 hour window
        sa.set_active("jared", "telegram", "old conversation");
        // Force last_active to be 2 hours ago
        if let Some(entry) = sa.entries.get_mut("jared") {
            entry.last_active = Utc::now() - chrono::Duration::hours(2);
        }
        let output = sa.render("other", 500);
        assert!(!output.contains("jared"));
    }

    #[test]
    fn set_idle_marks_correctly() {
        let mut sa = SituationalAwareness::new(24);
        sa.set_active("jared", "telegram", "test");
        sa.set_idle("jared");
        assert_eq!(sa.entries["jared"].status, ConvStatus::Idle);
    }
}
