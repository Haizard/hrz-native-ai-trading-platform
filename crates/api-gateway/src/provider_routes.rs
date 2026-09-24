//! `/agent/provider-config` -- a user's own AI provider (`docs/09`).
//!
//! ## What this is for
//!
//! The deployment has one **primary** model, set once in its environment
//! (`AI_PROVIDER` / `AI_MODEL` / `AI_API_KEY`). A user who wants their own
//! model -- their own OpenAI key, their own Claude, an OpenRouter account --
//! stores the configuration **here**, and from then on every `/agent/*` call
//! they make runs through their provider instead of the deployment's.
//!
//! ## The key is sealed before it is stored
//!
//! The API key travels in the request body once, is sealed with the platform's
//! key-encryption key (`BROKER_KEK`, the same vault that seals exchange API
//! keys), and is never returned to any client. `GET` answers with the
//! provider, the model and *whether* a key is stored -- never the key. The
//! sealed blob is bound to `(user_id, "ai-provider")`, so a row moved between
//! users fails to decrypt rather than decrypting into somebody else's key.
//!
//! ## One route, one owner
//!
//! Every route takes the authenticated user and scopes on them. There is no
//! admin path and no id parameter: a config is a user's own setting, and the
//! ownership check is the authentication itself.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use ai_agent::{Agent, OpenAiCompatConfig, ProviderId};

use crate::auth::UserContext;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::AppState;

/// The AAD scope name a stored key is sealed under.
///
/// Distinct from the broker credential scope on purpose: the two kinds of
/// secret live in different tables, and sharing an AAD string would let a
/// broker ciphertext be opened as an AI key and vice versa. The vault binds
/// the *user and this label*, so a blob copied from `broker_accounts` is
/// refused rather than reused.
const SCOPE: &str = "ai-provider";

/// `GET /agent/provider-config`'s response.
///
/// Everything the settings panel needs, minus the one field it must never
/// receive. `has_key` is a boolean on purpose: a client cannot "unsee" a key
/// it was sent, so the API's answer is shaped so it never has one.
#[derive(Debug, Serialize)]
pub struct ProviderConfigView {
    /// Provider wire name, e.g. `openai`.
    pub provider: String,
    /// Model id as the endpoint spells it.
    pub model_id: String,
    /// Endpoint override, when the user set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Whether a key is stored (never the key itself).
    pub has_key: bool,
    /// When it was last changed, unix nanoseconds.
    pub updated_at: i64,
}

/// `PUT /agent/provider-config`'s body.
#[derive(Debug, Deserialize)]
pub struct PutProviderConfig {
    /// Provider wire name; one of the names `/capabilities`' provider list
    /// spells out (see [`ProviderId::from_name`] for the accepted aliases).
    pub provider: String,
    /// Model id, e.g. `gpt-4o`, `deepseek-chat`.
    pub model_id: String,
    /// The API key. Required for every provider except `bedrock`, which
    /// authenticates with the deployment's AWS credentials and cannot be
    /// overridden per user.
    pub api_key: Option<String>,
    /// Optional endpoint override, for a self-hosted or proxied gateway.
    pub base_url: Option<String>,
    /// Extra headers the endpoint requires, as a JSON object of
    /// header-name -> value.
    #[serde(default)]
    pub extra_headers: serde_json::Value,
}

/// The user's config as the agent layer can use it, key already decrypted.
pub(crate) struct DecryptedProviderConfig {
    pub provider: ProviderId,
    pub model_id: String,
    pub api_key: Zeroizing<String>,
    pub base_url: Option<String>,
    pub extra_headers: Vec<(String, String)>,
}

/// `GET /agent/provider-config`
pub async fn get(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<Json<ProviderConfigView>, ApiError> {
    let database = state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the database is not configured; AI settings cannot be stored on this deployment",
        )
    })?;
    let Some(row) = db::find_provider_config(database.pool(), user.user_id).await? else {
        return Err(ApiError::coded(
            StatusCode::NOT_FOUND,
            "NO_PROVIDER_CONFIG",
            "you have not stored an AI provider configuration; the deployment's primary model \
             applies",
        ));
    };
    Ok(Json(ProviderConfigView {
        provider: row.provider,
        model_id: row.model_id,
        base_url: row.base_url,
        // The key's existence is provable without reading it back: the row
        // exists, and the column is NOT NULL.
        has_key: true,
        updated_at: row.updated_at,
    }))
}

/// `PUT /agent/provider-config`
///
/// Seals the key and stores the config in one write. An existing config is
/// replaced wholesale -- a provider config is a setting, not a history.
pub async fn put(
    State(state): State<AppState>,
    user: UserContext,
    ApiJson(body): ApiJson<PutProviderConfig>,
) -> Result<Json<ProviderConfigView>, ApiError> {
    // Both dependencies are required, and each refusal names its own fix:
    // "no database" and "no vault" are different operator actions, and a
    // merged message would send the user hunting for one when the other is
    // the actual answer.
    let database = state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the database is not configured; AI settings cannot be stored on this deployment",
        )
    })?;
    let vault = vault(&state)?;

    let provider = ProviderId::from_name(&body.provider).ok_or_else(|| {
        ApiError::bad_request(
            "PROVIDER_UNKNOWN",
            format!(
                "`{}` is not a known provider. Known: bedrock, openai, anthropic, openrouter, \
                 deepseek, grok, huggingface, openai-compat.",
                body.provider
            ),
        )
    })?;

    // Bedrock per-user makes no sense: its credentials are the deployment's
    // IAM identity, not a key a user can bring. Refusing here beats storing a
    // config that cannot work.
    if provider == ProviderId::Bedrock {
        return Err(ApiError::bad_request(
            "PROVIDER_UNSUPPORTED",
            "bedrock authenticates with the deployment's AWS credentials and cannot be \
             overridden per user; choose an endpoint provider instead",
        ));
    }

    let model_id = body.model_id.trim().to_string();
    if model_id.is_empty() {
        return Err(ApiError::bad_request(
            "MODEL_REQUIRED",
            "`model_id` is required and cannot be empty",
        ));
    }

    let api_key = body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "API_KEY_REQUIRED",
                format!(
                    "`api_key` is required for the `{}` provider",
                    provider.wire_name()
                ),
            )
        })?;

    let extra_headers = parse_extra_headers(&body.extra_headers)?;
    let _ = extra_headers; // validated here; serialised from `body` on the write

    // Sealed before it touches the database; the plaintext lives exactly as
    // long as this function and dies with the request.
    let scope = trading_engine::SecretScope::new(user.user_id, SCOPE);
    let ciphertext = vault.seal(&scope, api_key.as_bytes())?;

    db::upsert_provider_config(
        database.pool(),
        user.user_id,
        &db::NewProviderConfig {
            provider: provider.wire_name(),
            model_id: &model_id,
            api_key_ciphertext: ciphertext,
            base_url: body
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty()),
            extra_headers: &body.extra_headers,
            kek_fingerprint: vault.fingerprint(),
        },
    )
    .await?;

    tracing::info!(
        target: "api_gateway",
        user = %user.user_id,
        provider = provider.wire_name(),
        model = %model_id,
        "user provider config stored"
    );

    Ok(Json(ProviderConfigView {
        provider: provider.wire_name().to_string(),
        model_id,
        base_url: body
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(str::to_string),
        has_key: true,
        updated_at: crate::now_ns(),
    }))
}

/// `DELETE /agent/provider-config`
///
/// Idempotent: deleting when nothing is stored is still a success, because
/// the state the client wanted ("no override") already holds.
pub async fn delete(
    State(state): State<AppState>,
    user: UserContext,
) -> Result<StatusCode, ApiError> {
    let database = state.db.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "the database is not configured; AI settings cannot be stored on this deployment",
        )
    })?;
    db::delete_provider_config(database.pool(), user.user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Build the user's provider client from their stored config.
///
/// `Ok(None)` means "no override stored" -- the caller falls back to the
/// deployment primary. `Err` means an override exists but cannot be used,
/// which is reported by name (`PROVIDER_NOT_USABLE`) so the client knows the
/// stored config, not the deployment, is the problem.
pub(crate) async fn decrypted_config(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<DecryptedProviderConfig>, ApiError> {
    let Some(database) = &state.db else {
        return Ok(None);
    };
    let Some(row) = db::find_provider_config(database.pool(), user_id).await? else {
        return Ok(None);
    };
    let Some(vault) = &state.vault else {
        // A stored config with no vault cannot be opened. This is a boot-order
        // mismatch (the row predates the variable being unset, say), and the
        // honest answer is a refusal that names it, not a silent fallback that
        // would run someone else's model than the one they configured.
        return Err(ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "PROVIDER_NOT_USABLE",
            "a provider configuration is stored but the key-encryption key is not available; \
             the deployment cannot open it (BROKER_KEK unset)",
        ));
    };

    let Some(sealed) = db::provider_config_key(database.pool(), user_id).await? else {
        // The config row and its key are written together; a missing key means
        // the row is corrupt. Say so rather than falling back.
        return Err(ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "PROVIDER_NOT_USABLE",
            "the stored provider configuration has no key material; re-save it in AI settings",
        ));
    };

    let scope = trading_engine::SecretScope::new(user_id, SCOPE);
    let opened = vault
        .open(&scope, &sealed.api_key_ciphertext)
        .map_err(|_| {
            // The vault's message names the failure modes; none of them is
            // safe to repeat to the client. "Re-save it" covers every one.
            ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "PROVIDER_NOT_USABLE",
                "the stored API key could not be opened (it was sealed with a different \
             key-encryption key, or the row was altered); re-save it in AI settings",
            )
        })?;
    let api_key = Zeroizing::new(String::from_utf8((*opened).to_vec()).map_err(|_| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "PROVIDER_NOT_USABLE",
            "the stored API key is not valid text; re-save it in AI settings",
        )
    })?);

    let provider = ProviderId::from_name(&row.provider).ok_or_else(|| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "PROVIDER_NOT_USABLE",
            format!(
                "the stored provider `{}` is not known to this build; re-save the configuration",
                row.provider
            ),
        )
    })?;

    let extra_headers = parse_extra_headers(&row.extra_headers)?;

    Ok(Some(DecryptedProviderConfig {
        provider,
        model_id: row.model_id,
        api_key,
        base_url: row.base_url,
        extra_headers,
    }))
}

/// Resolve the `Agent` to run for this user: their own provider when one is
/// stored and usable, the deployment primary otherwise.
///
/// This is the single funnel every agent path goes through -- `POST
/// /agent/ask`, `POST /agent/generate-strategy` and the agent socket -- so
/// the precedence rule exists in exactly one place and cannot drift between
/// transports.
pub(crate) async fn resolve_agent(
    state: &AppState,
    user: &UserContext,
) -> Result<Arc<Agent>, ApiError> {
    match decrypted_config(state, user.user_id).await? {
        Some(config) => {
            let mut llm_config = OpenAiCompatConfig::for_provider(
                config.provider,
                &config.model_id,
                &config.api_key,
            );
            if let Some(base_url) = &config.base_url {
                llm_config.base_url = base_url.clone();
            }
            llm_config.extra_headers = config.extra_headers;
            let client = ai_agent::OpenAiCompatClient::new(llm_config).map_err(|e| {
                ApiError::coded(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PROVIDER_NOT_USABLE",
                    e.to_string(),
                )
            })?;
            tracing::debug!(
                target: "api_gateway",
                user = %user.user_id,
                provider = config.provider.wire_name(),
                model = %config.model_id,
                "agent running on the user's own provider"
            );
            Ok(Arc::new(Agent::new(
                Arc::new(client),
                (*state.skills).clone(),
                ai_agent::AgentConfig::default(),
            )))
        }
        None => state.agent.clone().ok_or_else(|| {
            ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "AGENT_NOT_CONFIGURED",
                "no AI model is available: you have not stored a provider configuration, and \
                 the deployment has no primary model (set AI_PROVIDER/AI_MODEL/AI_API_KEY, or \
                 add your own key in AI settings)",
            )
        }),
    }
}

/// The vault, or the 503 that says what to set.
fn vault(state: &AppState) -> Result<&Arc<trading_engine::SecretVault>, ApiError> {
    state.vault.as_ref().ok_or_else(|| {
        ApiError::unavailable("API keys cannot be stored: BROKER_KEK is not set on this deployment")
    })
}

/// Validate and flatten the extra-headers object.
///
/// The input is a JSON object of strings, because that is what the settings
/// panel sends and what the client ultimately needs back. A non-object, a
/// non-string value, or a header name with a newline (a header-injection
/// vector) is refused here rather than at request time on the provider call,
/// where the error would arrive after a paid run had started.
fn parse_extra_headers(raw: &serde_json::Value) -> Result<Vec<(String, String)>, ApiError> {
    let Some(map) = raw.as_object() else {
        return Err(ApiError::bad_request(
            "HEADERS_INVALID",
            "`extra_headers` must be a JSON object of header-name -> string-value",
        ));
    };
    let mut out = Vec::with_capacity(map.len());
    for (name, value) in map {
        let value = value.as_str().ok_or_else(|| {
            ApiError::bad_request(
                "HEADERS_INVALID",
                format!("`extra_headers[\"{name}\"]` must be a string"),
            )
        })?;
        if name.is_empty() || name.contains(['\r', '\n', ':']) {
            return Err(ApiError::bad_request(
                "HEADERS_INVALID",
                format!("`extra_headers[\"{name}\"]` is not a valid header name"),
            ));
        }
        if value.contains(['\r', '\n']) {
            return Err(ApiError::bad_request(
                "HEADERS_INVALID",
                format!("`extra_headers[\"{name}\"]` is not a valid header value"),
            ));
        }
        out.push((name.clone(), value.to_string()));
    }
    Ok(out)
}
