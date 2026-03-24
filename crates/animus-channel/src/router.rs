//! MessageRouter — triage, priority scoring, and thread routing.
//!
//! Every inbound ChannelMessage passes through the router before being
//! dispatched to a reasoning thread. The router:
//!   1. Runs the injection scanner (blocks injected content)
//!   2. Scores priority (urgency heuristics + optional LLM triage call)
//!   3. Decides thread routing (existing thread vs new thread)

use crate::message::{ChannelMessage, MessagePriority};
use crate::scanner::{InjectionScanner, ScanResult};
use std::sync::Arc;

/// Result of routing a message.
#[derive(Debug)]
pub enum RouteDecision {
    /// Dispatch to this reasoning thread ID.
    ExistingThread(String),
    /// Spawn a new thread with this suggested name.
    NewThread(String),
    /// Message was blocked by injection scanner. Contains the alert.
    InjectionBlocked(InjectionAlert),
}

/// Details about a detected injection attempt.
#[derive(Debug, Clone)]
pub struct InjectionAlert {
    pub channel_id: String,
    pub thread_id: String,
    pub sender_name: String,
    pub excerpt: String,
    pub confidence: f32,
}

/// The message router — triage, priority, thread routing.
pub struct MessageRouter {
    scanner: Arc<InjectionScanner>,
}

impl MessageRouter {
    pub fn new(scanner: Arc<InjectionScanner>) -> Self {
        Self { scanner }
    }

    /// Process an inbound message: scan for injection, score priority, decide routing.
    /// Mutates the message's priority field.
    pub async fn route(&self, mut msg: ChannelMessage) -> (ChannelMessage, RouteDecision) {
        // Step 1: Injection scan (skip for trusted senders with high trust)
        if !msg.sender.is_trusted {
            let content = msg.text.clone().unwrap_or_default();
            let scan = self.scanner.scan(&content, &msg.sender.channel_user_id).await;
            if matches!(scan, ScanResult::Injected { .. }) {
                let confidence = if let ScanResult::Injected { confidence } = scan {
                    confidence
                } else {
                    1.0
                };
                let excerpt = msg
                    .text
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect::<String>();
                let alert = InjectionAlert {
                    channel_id: msg.channel_id.clone(),
                    thread_id: msg.thread_id.clone(),
                    sender_name: msg.sender.name.clone(),
                    excerpt,
                    confidence,
                };
                tracing::warn!(
                    "Injection detected from {} via {} (confidence={:.2})",
                    msg.sender.name,
                    msg.channel_id,
                    confidence
                );
                return (msg, RouteDecision::InjectionBlocked(alert));
            }
        }

        // Step 2: Priority scoring (heuristic — LLM triage is a future enhancement)
        msg.priority = self.score_priority(&msg);

        // Step 3: Thread routing — use thread_id as the routing key
        // In future: semantic match against active thread contexts in VectorFS
        let thread_key = format!("{}:{}", msg.channel_id, msg.thread_id);
        let decision = RouteDecision::ExistingThread(thread_key.clone());

        (msg, decision)
    }

    /// Heuristic priority scoring. Returns MessagePriority based on content signals.
    fn score_priority(&self, msg: &ChannelMessage) -> MessagePriority {
        let text = msg.text.as_deref().unwrap_or("").to_lowercase();

        // Trusted senders get boosted priority
        let base = if msg.sender.is_trusted {
            MessagePriority::Normal
        } else {
            MessagePriority::Low
        };

        // Urgency keywords
        let urgent_keywords = [
            "urgent", "emergency", "asap", "critical", "immediately",
            "help", "broken", "down", "alert", "alarm",
        ];
        let has_urgent = urgent_keywords.iter().any(|kw| text.contains(kw));

        // Question or direct address signals Normal
        let is_question = text.contains('?') || text.starts_with("hey") || text.starts_with("animus");

        if has_urgent && msg.sender.is_trusted {
            MessagePriority::Critical
        } else if has_urgent {
            MessagePriority::High
        } else if is_question || msg.sender.is_trusted {
            MessagePriority::Normal
        } else {
            base
        }
    }
}
