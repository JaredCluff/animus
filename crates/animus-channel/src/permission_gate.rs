//! Permission gate — structured handler for `animus.in.permission_request`.
//!
//! Claude Code instances call `request_permission(action, details)` which sends a
//! NATS request to `animus.in.permission_request` and awaits a JSON reply:
//!   `{"approved": true, "reason": "..."}` or `{"approved": false, "reason": "..."}`
//!
//! This module intercepts that subject with a dedicated subscription (separate from
//! the general ChannelBus routing), evaluates the request using the current autonomy
//! mode, and either auto-approves, auto-denies, or escalates to Telegram for human
//! input.
//!
//! # Autonomy policy
//! - `Full`        — auto-approve everything
//! - `GoalDirected`— auto-approve low-risk actions; escalate destructive/sensitive ones
//! - `Reactive`    — always escalate to Telegram
//!
//! If Telegram is not configured, escalations are auto-denied.
//! If the user doesn't respond within `escalation_timeout_secs`, the request is denied.

use crate::bus::ChannelBus;
use crate::telegram::api::TelegramClient;
use animus_core::config::AutonomyMode;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{oneshot, watch, Mutex};

/// NATS subject that Claude Code instances publish permission requests to.
pub const PERMISSION_REQUEST_SUBJECT: &str = "animus.in.permission_request";

/// How long to wait for a Telegram yes/no reply before auto-denying (seconds).
const DEFAULT_ESCALATION_TIMEOUT_SECS: u64 = 60;

// ── Policy ────────────────────────────────────────────────────────────────────

/// Low-risk action types that are safe to auto-approve in GoalDirected mode.
const AUTO_APPROVE_ACTIONS: &[&str] = &[
    "network_request",
    "read_file",
    "list_directory",
    "search_files",
    "nats_publish",
    "kv_get",
    "kv_keys",
    "js_stream_info",
];

/// Decision reached after evaluating a permission request against policy.
#[derive(Debug)]
enum GateDecision {
    Approve { reason: String },
    Deny { reason: String },
    Escalate,
}

fn evaluate_policy(action: &str, mode: AutonomyMode) -> GateDecision {
    match mode {
        AutonomyMode::Full => GateDecision::Approve {
            reason: "full autonomy mode — auto-approved".into(),
        },
        AutonomyMode::GoalDirected => {
            if AUTO_APPROVE_ACTIONS.contains(&action) {
                GateDecision::Approve {
                    reason: "low-risk operation, goal_directed mode".into(),
                }
            } else {
                GateDecision::Escalate
            }
        }
        AutonomyMode::Reactive => GateDecision::Escalate,
    }
}

// ── PermissionGate ────────────────────────────────────────────────────────────

/// Structured handler for `animus.in.permission_request`.
///
/// Obtained via [`PermissionGate::new`] and started by calling [`PermissionGate::start`].
pub struct PermissionGate {
    nats_client: async_nats::Client,
    telegram_client: Option<Arc<TelegramClient>>,
    telegram_chat_id: Option<i64>,
    autonomy_rx: watch::Receiver<AutonomyMode>,
    escalation_timeout_secs: u64,
    /// Oneshot sender waiting for a yes/no Telegram reply.
    /// Only one escalation can be pending at a time.
    pending_escalation: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
}

impl PermissionGate {
    /// Create a new permission gate.
    ///
    /// - `nats_client`        — a connected NATS client (can be cloned from another)
    /// - `telegram_client`    — Telegram client for escalation (optional)
    /// - `telegram_chat_id`   — trusted chat to send escalation messages to
    /// - `autonomy_rx`        — shared watch receiver so the gate observes live mode changes
    pub fn new(
        nats_client: async_nats::Client,
        telegram_client: Option<Arc<TelegramClient>>,
        telegram_chat_id: Option<i64>,
        autonomy_rx: watch::Receiver<AutonomyMode>,
    ) -> Self {
        Self {
            nats_client,
            telegram_client,
            telegram_chat_id,
            autonomy_rx,
            escalation_timeout_secs: DEFAULT_ESCALATION_TIMEOUT_SECS,
            pending_escalation: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the permission gate. Spawns two background tasks:
    /// 1. NATS subscription on `animus.in.permission_request` — handles requests
    /// 2. ChannelBus subscription — watches for Telegram "yes"/"no" replies
    pub async fn start(self: Arc<Self>, bus: Arc<ChannelBus>) {
        // Task 1: handle incoming permission requests from NATS
        let gate = self.clone();
        match gate.nats_client.subscribe(PERMISSION_REQUEST_SUBJECT).await {
            Ok(mut sub) => {
                tokio::spawn(async move {
                    tracing::info!("PermissionGate: listening on '{PERMISSION_REQUEST_SUBJECT}'");
                    while let Some(msg) = sub.next().await {
                        gate.handle_request(msg).await;
                    }
                    tracing::warn!("PermissionGate: NATS subscription ended");
                });
            }
            Err(e) => {
                tracing::warn!("PermissionGate: failed to subscribe to '{PERMISSION_REQUEST_SUBJECT}': {e}");
                return;
            }
        }

        // Task 2: watch ChannelBus for Telegram "yes"/"no" replies
        let gate2 = self.clone();
        let mut channel_rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                match channel_rx.recv().await {
                    Ok(msg) => {
                        if msg.channel_id != "telegram" {
                            continue;
                        }
                        // Only accept approval from the configured trusted chat_id.
                        // This prevents any random Telegram user from approving escalations.
                        if let Some(trusted_id) = gate2.telegram_chat_id {
                            if msg.thread_id != trusted_id.to_string() {
                                tracing::warn!(
                                    "PermissionGate: approval attempt from untrusted chat {} — ignoring",
                                    msg.thread_id
                                );
                                continue;
                            }
                        }
                        let text = match &msg.text {
                            Some(t) => t.trim().to_lowercase(),
                            None => continue,
                        };

                        // Only intercept if there is a pending escalation
                        let maybe_sender = {
                            let mut guard = gate2.pending_escalation.lock().await;
                            if guard.is_none() {
                                None
                            } else if text.starts_with("yes")
                                || text.starts_with("approve")
                                || text.starts_with("ok")
                                || text == "y"
                            {
                                guard.take().map(|s| (s, true))
                            } else if text.starts_with("no")
                                || text.starts_with("deny")
                                || text.starts_with("reject")
                                || text == "n"
                            {
                                guard.take().map(|s| (s, false))
                            } else {
                                None
                            }
                        };

                        if let Some((sender, approved)) = maybe_sender {
                            tracing::info!(
                                approved,
                                "PermissionGate: Telegram reply received — {}",
                                if approved { "approved" } else { "denied" }
                            );
                            let _ = sender.send(approved);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("PermissionGate: channel_rx lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Handle a single `animus.in.permission_request` message.
    async fn handle_request(&self, msg: async_nats::Message) {
        let reply_inbox = match msg.reply.clone() {
            Some(r) => r,
            None => {
                tracing::warn!("PermissionGate: permission request missing reply inbox, ignoring");
                return;
            }
        };

        // Parse the request payload
        let payload_str = match std::str::from_utf8(&msg.payload) {
            Ok(s) => s,
            Err(_) => {
                self.send_denial(&reply_inbox, "non-UTF8 payload").await;
                return;
            }
        };

        let req: serde_json::Value = match serde_json::from_str(payload_str) {
            Ok(v) => v,
            Err(e) => {
                self.send_denial(&reply_inbox, &format!("invalid JSON: {e}")).await;
                return;
            }
        };

        let request_id = req["request_id"].as_str().unwrap_or("unknown").to_string();
        let from = req["from"].as_str().unwrap_or("unknown").to_string();
        let action = req["action"].as_str().unwrap_or("").to_string();
        let details = req["details"].as_str().unwrap_or("(no details)").to_string();

        if action.is_empty() {
            self.send_denial(&reply_inbox, "missing action field").await;
            return;
        }

        let mode = *self.autonomy_rx.borrow();
        let decision = evaluate_policy(&action, mode);

        tracing::info!(
            request_id = %request_id,
            from = %from,
            action = %action,
            mode = ?mode,
            decision = ?decision,
            "PermissionGate: evaluating request"
        );

        match decision {
            GateDecision::Approve { reason } => {
                self.send_approval(&reply_inbox, &reason).await;
            }
            GateDecision::Deny { reason } => {
                self.send_denial(&reply_inbox, &reason).await;
            }
            GateDecision::Escalate => {
                self.escalate_to_telegram(
                    &reply_inbox,
                    &request_id,
                    &from,
                    &action,
                    &details,
                )
                .await;
            }
        }
    }

    /// Escalate a request to Telegram and await a yes/no reply.
    async fn escalate_to_telegram(
        &self,
        reply_inbox: &str,
        request_id: &str,
        from: &str,
        action: &str,
        details: &str,
    ) {
        // If another escalation is already pending, deny this one immediately
        {
            let guard = self.pending_escalation.lock().await;
            if guard.is_some() {
                tracing::warn!(
                    request_id,
                    "PermissionGate: another escalation already pending — denying"
                );
                self.send_denial(reply_inbox, "another permission request is already pending — try again shortly").await;
                return;
            }
        }

        let Some(ref telegram) = self.telegram_client else {
            tracing::warn!(request_id, "PermissionGate: Telegram not configured — auto-denying escalation");
            self.send_denial(reply_inbox, "no supervisor configured for this escalation").await;
            return;
        };

        let Some(chat_id) = self.telegram_chat_id else {
            tracing::warn!(request_id, "PermissionGate: no Telegram chat_id — auto-denying escalation");
            self.send_denial(reply_inbox, "no supervisor chat configured").await;
            return;
        };

        let tg_msg = format!(
            "🔐 **Permission Request** `[{request_id}]`\n\n\
             **From:** `{from}`\n\
             **Action:** `{action}`\n\n\
             {details}\n\n\
             Reply **yes** to approve or **no** to deny.\n\
             _(timeout: {}s)_",
            self.escalation_timeout_secs
        );

        if let Err(e) = telegram.send_message(chat_id, &tg_msg, None).await {
            tracing::warn!(request_id, "PermissionGate: Telegram send failed ({e}) — auto-denying");
            self.send_denial(reply_inbox, "failed to reach supervisor").await;
            return;
        }

        tracing::info!(request_id, "PermissionGate: escalated to Telegram chat {chat_id}");

        // Register oneshot for reply
        let (tx, rx) = oneshot::channel::<bool>();
        {
            let mut guard = self.pending_escalation.lock().await;
            *guard = Some(tx);
        }

        let timeout = tokio::time::Duration::from_secs(self.escalation_timeout_secs);
        let result = tokio::time::timeout(timeout, rx).await;

        // Clear pending escalation regardless of outcome
        {
            let mut guard = self.pending_escalation.lock().await;
            *guard = None;
        }

        match result {
            Ok(Ok(true)) => {
                self.send_approval(reply_inbox, "approved via Telegram").await;
            }
            Ok(Ok(false)) => {
                self.send_denial(reply_inbox, "denied via Telegram").await;
            }
            Ok(Err(_)) => {
                // Sender dropped (shouldn't happen)
                self.send_denial(reply_inbox, "escalation channel closed unexpectedly").await;
            }
            Err(_) => {
                tracing::warn!(request_id, "PermissionGate: escalation timed out after {}s", self.escalation_timeout_secs);
                // Notify Telegram that the request timed out
                let timeout_msg = format!(
                    "⏱ Permission request `[{request_id}]` timed out — auto-denied."
                );
                let _ = telegram.send_message(chat_id, &timeout_msg, None).await;
                self.send_denial(
                    reply_inbox,
                    &format!(
                        "timed out after {}s — no response from supervisor",
                        self.escalation_timeout_secs
                    ),
                )
                .await;
            }
        }
    }

    async fn send_approval(&self, reply_inbox: &str, reason: &str) {
        let body = serde_json::json!({ "approved": true, "reason": reason }).to_string();
        self.nats_reply(reply_inbox, &body).await;
        tracing::info!("PermissionGate: approved — {reason}");
    }

    async fn send_denial(&self, reply_inbox: &str, reason: &str) {
        let body = serde_json::json!({ "approved": false, "reason": reason }).to_string();
        self.nats_reply(reply_inbox, &body).await;
        tracing::info!("PermissionGate: denied — {reason}");
    }

    async fn nats_reply(&self, subject: &str, body: &str) {
        if let Err(e) = self
            .nats_client
            .publish(subject.to_string(), body.as_bytes().to_vec().into())
            .await
        {
            tracing::warn!("PermissionGate: failed to publish reply to '{subject}': {e}");
        }
    }
}
