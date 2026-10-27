//! The payment-provider seam: billing mode, webhook verification and events.
//!
//! ## Why a seam rather than a provider
//!
//! No live provider credentials exist in this deployment, and the subscription
//! still has to work end to end. So the lifecycle is **internal** by default:
//! `POST /billing/subscribe` starts a trial or a period, and the database is the
//! system of record. The provider seam is the small surface a real integration
//! plugs into: a signed webhook (`POST /billing/webhook`) and an event shape
//! that is already idempotent on `(provider, event_id)`.
//!
//! `BILLING_MODE=provider` switches the *source of truth* to webhooks: the
//! subscribe route stops granting access itself and expects the provider's
//! `subscription.activated` event to do it. Until a provider client is written,
//! that route answers 503 rather than pretending a checkout exists.
//!
//! ## The signature is checked before the body is parsed
//!
//! An unsigned webhook is an unauthenticated request that can grant a paid
//! plan. Verification therefore happens first, over the raw bytes, and the
//! comparison is constant-time. Parsing first would let an attacker learn the
//! event schema from error messages without proving they can sign anything.
//!
//! ## Replay is handled by the database, not here
//!
//! [`WebhookEvent`] has no dedupe logic. `db::record_billing_event` owns that:
//! the unique `(provider, event_id)` insert decides whether an event is new, so
//! two concurrent deliveries cannot both apply it.

use std::sync::Arc;

use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::BillingError;

type HmacSha256 = Hmac<Sha256>;

/// The header a provider signs a webhook with.
///
/// A `sha256=` prefix is accepted as well as a bare hex digest, because that is
/// the shape most providers use and stripping it is cheaper than making an
/// operator discover the difference from a 401.
pub const SIGNATURE_HEADER: &str = "x-billing-signature";

/// Where the subscription lifecycle is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BillingMode {
    /// No external provider: subscribe/cancel/resume act directly on the
    /// database. Suitable for development, demos and a self-hosted install.
    Internal,
    /// An external provider is the system of record; webhooks activate and
    /// renew subscriptions.
    Provider,
}

impl BillingMode {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Provider => "provider",
        }
    }
}

/// Billing configuration read from the environment.
#[derive(Clone)]
pub struct BillingConfig {
    /// Which lifecycle is authoritative.
    pub mode: BillingMode,
    /// The webhook HMAC secret, when one is configured.
    webhook_secret: Option<Arc<Vec<u8>>>,
}

impl std::fmt::Debug for BillingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BillingConfig")
            .field("mode", &self.mode)
            .field(
                "webhook_secret",
                &if self.webhook_secret.is_some() {
                    "<configured>"
                } else {
                    "<absent>"
                },
            )
            .finish()
    }
}

impl BillingConfig {
    /// Read `BILLING_MODE` and `BILLING_WEBHOOK_SECRET` from the environment.
    ///
    /// The default is `internal`, and that default is deliberate: a deployment
    /// with no provider is still fully usable, and the alternative -- refusing
    /// to start or disabling every paid feature -- would make the product look
    /// broken until a merchant account exists.
    #[must_use]
    pub fn from_env() -> Self {
        let mode = match std::env::var("BILLING_MODE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "provider" => BillingMode::Provider,
            _ => BillingMode::Internal,
        };
        let secret = std::env::var("BILLING_WEBHOOK_SECRET")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| Arc::new(value.into_bytes()));
        Self {
            mode,
            webhook_secret: secret,
        }
    }

    /// An explicitly internal configuration, for tests.
    #[must_use]
    pub fn internal() -> Self {
        Self {
            mode: BillingMode::Internal,
            webhook_secret: None,
        }
    }

    /// Whether a webhook secret is configured.
    #[must_use]
    pub fn has_webhook_secret(&self) -> bool {
        self.webhook_secret.is_some()
    }

    /// The webhook secret, for the service that verifies deliveries.
    ///
    /// Crate-visible rather than public: the secret must not be reachable from
    /// outside this module tree, and the verifier is the only consumer.
    #[must_use]
    pub(crate) fn webhook_secret(&self) -> Option<&Arc<Vec<u8>>> {
        self.webhook_secret.as_ref()
    }
}

/// A provider webhook, in the shape the seam accepts.
///
/// `id` is accepted as an alias for `event_id` because most providers spell the
/// dedupe key that way. `type` is the event kind (`subscription.activated`,
/// `subscription.renewed`, `subscription.canceled`, `subscription.expired`,
/// `payment.failed`).
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookEvent {
    /// The provider's unique event id.
    #[serde(alias = "id")]
    pub event_id: String,
    /// The event kind.
    #[serde(rename = "type")]
    pub kind: String,
    /// The user the event is about.
    pub user_id: Option<Uuid>,
    /// The plan the event refers to.
    pub plan_id: Option<String>,
    /// The billing cycle the event refers to.
    pub cycle: Option<String>,
    /// When the period the event describes ends.
    pub period_end: Option<DateTime<Utc>>,
    /// The provider's subscription id.
    pub provider_ref: Option<String>,
}

/// HMAC-SHA256 webhook verification.
#[derive(Clone)]
pub struct HmacProvider {
    provider: &'static str,
    secret: Arc<Vec<u8>>,
}

impl std::fmt::Debug for HmacProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HmacProvider")
            .field("provider", &self.provider)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl HmacProvider {
    /// Build a verifier over a shared secret.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            provider: "webhook",
            secret: Arc::new(secret.into()),
        }
    }

    /// The provider name recorded on billing events.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.provider
    }

    /// Verify the signature over the raw request body.
    ///
    /// # Errors
    /// Returns [`BillingError::InvalidSignature`] when the header is absent or
    /// does not match. The message never says which of the two happened.
    pub fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), BillingError> {
        let provided = headers
            .get(SIGNATURE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim())
            .map(|value| value.strip_prefix("sha256=").unwrap_or(value))
            .ok_or(BillingError::InvalidSignature)?;

        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| BillingError::InvalidSignature)?;
        mac.update(body);
        let expected = hex_lower(&mac.finalize().into_bytes());

        if expected
            .as_bytes()
            .ct_eq(provided.to_ascii_lowercase().as_bytes())
            .into()
        {
            Ok(())
        } else {
            Err(BillingError::InvalidSignature)
        }
    }

    /// Parse a verified webhook body.
    ///
    /// # Errors
    /// Returns [`BillingError::InvalidRequest`] for malformed JSON or a missing
    /// event id.
    pub fn parse(&self, body: &[u8]) -> Result<WebhookEvent, BillingError> {
        let event: WebhookEvent = serde_json::from_slice(body)
            .map_err(|error| BillingError::InvalidRequest(format!("webhook body: {error}")))?;
        if event.event_id.trim().is_empty() {
            return Err(BillingError::InvalidRequest(
                "webhook `event_id` (or `id`) is required".to_string(),
            ));
        }
        if event.kind.trim().is_empty() {
            return Err(BillingError::InvalidRequest(
                "webhook `type` is required".to_string(),
            ));
        }
        Ok(event)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).expect("key");
        mac.update(body);
        format!("sha256={}", hex_lower(&mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_a_valid_signature() {
        let body = br#"{"id":"evt_1","type":"subscription.activated"}"#;
        let mut headers = HeaderMap::new();
        headers.insert(SIGNATURE_HEADER, signed(b"secret", body).parse().unwrap());
        assert!(HmacProvider::new(b"secret".to_vec())
            .verify(&headers, body)
            .is_ok());
    }

    #[test]
    fn rejects_a_tampered_body() {
        let body = br#"{"id":"evt_1","type":"subscription.activated"}"#;
        let mut headers = HeaderMap::new();
        headers.insert(SIGNATURE_HEADER, signed(b"secret", body).parse().unwrap());
        assert!(HmacProvider::new(b"secret".to_vec())
            .verify(
                &headers,
                br#"{"id":"evt_2","type":"subscription.activated"}"#
            )
            .is_err());
    }

    #[test]
    fn rejects_a_missing_header() {
        assert!(HmacProvider::new(b"secret".to_vec())
            .verify(&HeaderMap::new(), b"{}")
            .is_err());
    }

    #[test]
    fn parses_provider_shaped_events() {
        let body = br#"{
            "id": "evt_1",
            "type": "subscription.activated",
            "user_id": "00000000-0000-0000-0000-000000000001",
            "plan_id": "pro",
            "cycle": "annual",
            "period_end": "2027-10-27T00:00:00Z"
        }"#;
        let event = HmacProvider::new(b"secret".to_vec()).parse(body).unwrap();
        assert_eq!(event.event_id, "evt_1");
        assert_eq!(event.plan_id.as_deref(), Some("pro"));
        assert_eq!(event.cycle.as_deref(), Some("annual"));
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let rendered = format!("{:?}", HmacProvider::new(b"super-secret".to_vec()));
        assert!(!rendered.contains("super-secret"));
    }
}
