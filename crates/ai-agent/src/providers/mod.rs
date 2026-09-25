//! Concrete [`LlmClient`](crate::llm_client::LlmClient) implementations, and
//! the provider registry that picks one.
//!
//! ## Two kinds of provider, one trait
//!
//! Every provider here answers the same [`LlmClient`] contract, and they fall
//! into two families by wire format:
//!
//! * **Converse** -- AWS Bedrock ([`bedrock`]), the original provider, whose
//!   tool-use contract this crate's message types were modelled on;
//! * **Chat Completions** -- [`openai_compat`], one adapter speaking for
//!   OpenAI, Anthropic, OpenRouter, DeepSeek, Grok/xAI, HuggingFace and any
//!   OpenAI-compatible gateway (Codex endpoints, vLLM, LiteLLM, Ollama).
//!
//! Adding a vendor that speaks Chat Completions is a [`ProviderId`] variant
//! and a base URL -- never a second translation of tool calls.
//!
//! ## Where the configuration comes from
//!
//! The **deployment's primary model** comes from the environment
//! ([`from_deployment_env`]): `AI_PROVIDER`, `AI_MODEL`, `AI_API_KEY`,
//! `AI_BASE_URL`. That is the model every user gets by default, set once per
//! deployment (Northflank env vars). A *user's own* provider is a stored,
//! encrypted config one layer up (the gateway's `/agent/provider-config`
//! routes) and overrides the primary per user; that layer constructs a client
//! from a [`crate::providers::openai_compat::OpenAiCompatConfig`] or a
//! [`BedrockClient`] directly, so this module's env path and that path share
//! the same client types and cannot drift.

pub mod bedrock;
pub mod openai_compat;

use crate::error::AgentError;

pub use bedrock::{BedrockClient, BedrockConfig};
pub use openai_compat::{AuthStyle, OpenAiCompatClient, OpenAiCompatConfig};

/// The providers this platform can drive.
///
/// The wire name is the stable identifier used in env vars, stored user
/// configs and logs; a renamed variant would silently orphan every stored
/// config, so treat the strings as frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    /// AWS Bedrock (Converse wire format).
    Bedrock,
    /// OpenAI.
    OpenAi,
    /// Anthropic (Chat Completions-compatible endpoint).
    Anthropic,
    /// OpenRouter (multi-model gateway).
    OpenRouter,
    /// DeepSeek.
    DeepSeek,
    /// xAI Grok.
    Grok,
    /// HuggingFace Inference providers.
    HuggingFace,
    /// Any other OpenAI-compatible endpoint (Codex gateways, vLLM, LiteLLM,
    /// Ollama, Azure OpenAI with `AI_BASE_URL`).
    OpenAiCompat,
}

impl ProviderId {
    /// The provider from its stable wire name; `None` when unknown.
    ///
    /// Accepts a few historic aliases so an operator typing `openai.com` or a
    /// user config carrying `xai` still resolves to the right provider.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "bedrock" | "aws-bedrock" | "aws_bedrock" => Some(Self::Bedrock),
            "openai" | "openai.com" => Some(Self::OpenAi),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "openrouter" => Some(Self::OpenRouter),
            "deepseek" => Some(Self::DeepSeek),
            "grok" | "xai" | "x.ai" => Some(Self::Grok),
            "huggingface" | "hf" | "hugging-face" => Some(Self::HuggingFace),
            "openai-compat" | "openai_compat" | "compatible" | "custom" => Some(Self::OpenAiCompat),
            _ => None,
        }
    }

    /// The stable wire name, as env vars and stored configs spell it.
    #[must_use]
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Bedrock => "bedrock",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::OpenRouter => "openrouter",
            Self::DeepSeek => "deepseek",
            Self::Grok => "grok",
            Self::HuggingFace => "huggingface",
            Self::OpenAiCompat => "openai-compat",
        }
    }

    /// The default API root for the Chat Completions family.
    ///
    /// `Bedrock` has none here -- it is not a Chat Completions endpoint, and
    /// its URL is built by its own client from the region.
    #[must_use]
    pub fn default_base_url(&self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::DeepSeek => "https://api.deepseek.com/v1",
            Self::Grok => "https://api.x.ai/v1",
            Self::HuggingFace => "https://router.huggingface.co/v1",
            Self::Bedrock | Self::OpenAiCompat => "",
        }
    }

    /// The model this deployment should default to when `AI_MODEL` is unset
    /// but the provider's key is present.
    ///
    /// Deliberately conservative: a model id that stops existing is a one-line
    /// env change, not a code change, and these are chosen to be the providers'
    /// stable flagship ids rather than snapshot dates.
    #[must_use]
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Bedrock => "",
            Self::OpenAi => "gpt-4o",
            Self::Anthropic => "claude-sonnet-4-5",
            Self::OpenRouter => "openai/gpt-4o",
            Self::DeepSeek => "deepseek-chat",
            Self::Grok => "grok-3",
            Self::HuggingFace => "meta-llama/Llama-3.3-70B-Instruct",
            Self::OpenAiCompat => "",
        }
    }

    /// Whether this provider can run without an API key.
    ///
    /// Only Bedrock can: it authenticates with AWS credentials via SigV4.
    /// Every Chat Completions provider needs a key, unless the operator has
    /// pointed `AI_BASE_URL` at a gateway that does its own auth.
    #[must_use]
    pub fn needs_api_key(&self) -> bool {
        !matches!(self, Self::Bedrock)
    }
}

/// Environment variable naming the deployment's primary provider.
pub const ENV_PROVIDER: &str = "AI_PROVIDER";
/// Environment variable naming the primary model id.
pub const ENV_MODEL: &str = "AI_MODEL";
/// Environment variable carrying the primary provider's API key.
pub const ENV_API_KEY: &str = "AI_API_KEY";
/// Environment variable overriding the primary provider's default base URL.
pub const ENV_BASE_URL: &str = "AI_BASE_URL";
/// Environment variable setting the primary provider's request timeout.
pub const ENV_TIMEOUT_SECS: &str = "AI_TIMEOUT_SECS";

/// The configured deployment-wide primary model, ready for the orchestrator.
///
/// Returned rather than an `Arc<dyn LlmClient>` so the caller can log the
/// provider/model pair and hold the client however it likes.
///
/// # Errors
/// [`AgentError::NotConfigured`] when nothing usable is configured; the
/// message names the exact variable or variables to set. Bedrock keeps its
/// own environment contract (`AWS_BEDROCK_*` + AWS credentials), so an
/// existing deployment that never sets `AI_PROVIDER` boots exactly as before.
pub fn from_deployment_env(
) -> Result<(std::sync::Arc<dyn crate::llm_client::LlmClient>, String), AgentError> {
    let requested = std::env::var(ENV_PROVIDER).ok();
    let provider = match requested.as_deref().map(str::trim) {
        // Unset or blank: Bedrock, the original provider. Preserving this
        // default is what makes the new variables purely additive -- every
        // deployment configured before they existed keeps working.
        None | Some("") => ProviderId::Bedrock,
        Some(name) => ProviderId::from_name(name).ok_or_else(|| {
            AgentError::NotConfigured(format!(
                "{ENV_PROVIDER} is `{name}`, which is not a known provider. Known: bedrock, \
                 openai, anthropic, openrouter, deepseek, grok, huggingface, openai-compat."
            ))
        })?,
    };

    let timeout_secs: u64 = std::env::var(ENV_TIMEOUT_SECS)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(180);

    match provider {
        ProviderId::Bedrock => {
            // The Bedrock path keeps its own env contract. Building through
            // `BedrockClient::from_env` means the error messages, the SigV4
            // details and the region defaults are defined in exactly one
            // place, and this registry adds no third spelling of them.
            let client = BedrockClient::from_env()?;
            let model = client.model_id().to_string();
            Ok((std::sync::Arc::new(client), model))
        }
        provider @ (ProviderId::OpenAi
        | ProviderId::Anthropic
        | ProviderId::OpenRouter
        | ProviderId::DeepSeek
        | ProviderId::Grok
        | ProviderId::HuggingFace
        | ProviderId::OpenAiCompat) => {
            // `provider` already carries the matched id here -- a shadowing
            // re-bind would be a no-op, and current clippy flags exactly that.
            let model = match std::env::var(ENV_MODEL) {
                Ok(model) if !model.trim().is_empty() => model.trim().to_string(),
                _ => provider.default_model().to_string(),
            };
            if model.is_empty() {
                return Err(AgentError::NotConfigured(format!(
                    "{ENV_MODEL} is not set and `{}` has no built-in default model. \
                     Set {ENV_MODEL} to the model id the endpoint expects.",
                    provider.wire_name()
                )));
            }

            let api_key = std::env::var(ENV_API_KEY)
                .map(|key| key.trim().to_string())
                .unwrap_or_default();
            if api_key.is_empty() && provider.needs_api_key() {
                return Err(AgentError::NotConfigured(format!(
                    "{ENV_API_KEY} is not set; the `{}` provider cannot be used without it. \
                     (Bedrock authenticates with AWS credentials instead.)",
                    provider.wire_name()
                )));
            }

            let mut config = OpenAiCompatConfig::for_provider(provider, &model, &api_key);
            config.timeout_secs = timeout_secs;
            if let Ok(base) = std::env::var(ENV_BASE_URL) {
                let base = base.trim().to_string();
                if !base.is_empty() {
                    config.base_url = base;
                }
            }
            // A custom base URL frequently means a gateway doing its own auth
            // (a local vLLM, a sidecar): an empty key is legitimate there, so
            // the key requirement above is only enforced against the provider
            // default, not against an operator-chosen endpoint.
            if provider == ProviderId::OpenAiCompat && config.base_url.is_empty() {
                return Err(AgentError::NotConfigured(format!(
                    "{ENV_PROVIDER} is `openai-compat`, which has no default endpoint. \
                     Set {ENV_BASE_URL} to the API root, e.g. https://gateway.internal/v1."
                )));
            }

            let model = config.model_id.clone();
            let client = OpenAiCompatClient::new(config)
                .map_err(|e| AgentError::NotConfigured(e.to_string()))?;
            Ok((std::sync::Arc::new(client), model))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for provider in [
            ProviderId::Bedrock,
            ProviderId::OpenAi,
            ProviderId::Anthropic,
            ProviderId::OpenRouter,
            ProviderId::DeepSeek,
            ProviderId::Grok,
            ProviderId::HuggingFace,
            ProviderId::OpenAiCompat,
        ] {
            assert_eq!(ProviderId::from_name(provider.wire_name()), Some(provider));
        }
    }

    #[test]
    fn aliases_and_casing_are_accepted() {
        assert_eq!(ProviderId::from_name("xai"), Some(ProviderId::Grok));
        assert_eq!(ProviderId::from_name("X.AI"), Some(ProviderId::Grok));
        assert_eq!(ProviderId::from_name("Claude"), Some(ProviderId::Anthropic));
        assert_eq!(ProviderId::from_name("  OpenAI "), Some(ProviderId::OpenAi));
        assert_eq!(
            ProviderId::from_name("custom"),
            Some(ProviderId::OpenAiCompat)
        );
        assert_eq!(ProviderId::from_name("nope"), None);
    }

    #[test]
    fn chat_providers_have_default_endpoints_and_models() {
        for provider in [
            ProviderId::OpenAi,
            ProviderId::Anthropic,
            ProviderId::OpenRouter,
            ProviderId::DeepSeek,
            ProviderId::Grok,
            ProviderId::HuggingFace,
        ] {
            assert!(provider.default_base_url().starts_with("https://"));
            assert!(!provider.default_model().is_empty());
            assert!(provider.needs_api_key());
        }
        // Bedrock is its own thing; openai-compat needs the operator to say
        // where to point.
        assert!(ProviderId::Bedrock.default_base_url().is_empty());
        assert!(!ProviderId::Bedrock.needs_api_key());
        assert!(ProviderId::OpenAiCompat.default_base_url().is_empty());
    }

    #[test]
    fn an_unknown_provider_name_names_the_known_set() {
        let _serial = env_serial();
        let guard = EnvGuard::set(&[(ENV_PROVIDER, "quantum-llm")]);
        let err = match from_deployment_env() {
            Ok(_) => panic!("must refuse an unknown provider"),
            Err(err) => err,
        };
        drop(guard);
        let message = err.to_string();
        assert!(message.contains("quantum-llm"), "{message}");
        assert!(message.contains("bedrock"), "{message}");
    }

    #[test]
    fn a_chat_provider_without_a_key_is_refused_by_name() {
        let _serial = env_serial();
        let guard = EnvGuard::set(&[
            (ENV_PROVIDER, "deepseek"),
            (ENV_MODEL, "deepseek-chat"),
            (ENV_API_KEY, ""),
        ]);
        let err = match from_deployment_env() {
            Ok(_) => panic!("no key, no provider"),
            Err(err) => err,
        };
        assert!(err.to_string().contains(ENV_API_KEY), "{err}");
        drop(guard);
    }

    #[test]
    fn an_openai_compat_provider_without_a_base_url_names_it() {
        let _serial = env_serial();
        let guard = EnvGuard::set(&[
            (ENV_PROVIDER, "openai-compat"),
            (ENV_MODEL, "whatever"),
            (ENV_API_KEY, "k"),
            (ENV_BASE_URL, ""),
        ]);
        let err = match from_deployment_env() {
            Ok(_) => panic!("no endpoint to call"),
            Err(err) => err,
        };
        assert!(err.to_string().contains(ENV_BASE_URL), "{err}");
        drop(guard);
    }

    /// The lock every env-mutating test holds for its whole body.
    ///
    /// `from_deployment_env` reads the process environment, which is global
    /// mutable state: two such tests running in parallel would each read the
    /// other's variables, and the failure would be an assertion two files
    /// apart blaming each other. Rust runs tests on threads, so a mutex is
    /// the standard fix -- hold it for the test's duration and the tests
    /// serialise.
    fn env_serial() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Serialises the env-var tests: they mutate process state that the rest
    /// of the crate's tests read through `from_deployment_env`.
    struct EnvGuard;

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
            Self
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for name in [
                ENV_PROVIDER,
                ENV_MODEL,
                ENV_API_KEY,
                ENV_BASE_URL,
                ENV_TIMEOUT_SECS,
            ] {
                std::env::remove_var(name);
            }
        }
    }
}
