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
//!   OpenAI, Anthropic, OpenRouter, DeepSeek, Grok/xAI, Groq, HuggingFace and any
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
    /// Groq inference API.
    Groq,
    /// HuggingFace Inference providers.
    HuggingFace,
    /// Google Gemini Developer API.
    Gemini,
    /// Cloudflare Workers AI / AI Gateway.
    Cloudflare,
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
            "groq" | "groqcloud" => Some(Self::Groq),
            "huggingface" | "hf" | "hugging-face" => Some(Self::HuggingFace),
            "gemini" | "google" | "google-ai" => Some(Self::Gemini),
            "cloudflare" | "workers-ai" => Some(Self::Cloudflare),
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
            Self::Groq => "groq",
            Self::HuggingFace => "huggingface",
            Self::Gemini => "gemini",
            Self::Cloudflare => "cloudflare",
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
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::HuggingFace => "https://router.huggingface.co/v1",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Self::Cloudflare => "",
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
            Self::Grok => "grok-4.7",
            Self::Groq => "qwen/qwen3.8-27b",
            Self::HuggingFace => "meta-llama/Llama-3.3-70B-Instruct",
            Self::Gemini => "gemini-3.8-flash",
            Self::Cloudflare => "@cf/meta/llama-4-scout-17b-16e-instruct",
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

/// Runs configured providers in order, skips models that cannot accept the
/// current image inputs, and tries the next provider after any provider error
/// (including exhausted quota, throttling and temporary outages).
pub struct FallbackClient {
    clients: Vec<std::sync::Arc<dyn crate::llm_client::LlmClient>>,
    name: String,
}

impl FallbackClient {
    /// Build a client that tries the supplied providers in order.
    #[must_use]
    pub fn new(clients: Vec<std::sync::Arc<dyn crate::llm_client::LlmClient>>) -> Self {
        let name = format!("fallback[{}]", clients.iter().map(|client| client.name()).collect::<Vec<_>>().join(" -> "));
        Self { clients, name }
    }
}

#[async_trait::async_trait]
impl crate::llm_client::LlmClient for FallbackClient {
    async fn complete(
        &self,
        request: crate::llm_client::LlmRequest,
    ) -> Result<crate::llm_client::LlmResponse, AgentError> {
        let needs_vision = request.messages.iter().flat_map(|message| &message.content).any(
            |block| matches!(block, crate::llm_client::ContentBlock::Image { .. }),
        );
        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        for client in &self.clients {
            if needs_vision && !client.supports_image_input() {
                skipped.push(client.name());
                continue;
            }
            match client.complete(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    tracing::warn!(target: "ai_agent", provider = client.name(), "AI provider failed; trying next configured provider");
                    failures.push(format!("{}: {error}", client.name()));
                }
            }
        }
        // Every rung answered, so what is worth keeping is which one answered
        // how. The last failure alone reads as "gemini is broken" in a chain
        // where gemini was the third try, and the operator cannot tell a
        // provider that refused from one that was never offered the request.
        if failures.is_empty() {
            if skipped.is_empty() {
                return Err(AgentError::Llm("no AI provider is configured".into()));
            }
            return Err(AgentError::Llm(format!(
                "the configured AI providers cannot process image input; the request asked \
                 for a picture and {} cannot read one. Configure a vision-capable model (a \
                 gemini-* or Cloudflare llama-4-scout rung, or a Grok 4.x).",
                skipped.join(", ")
            )));
        }
        let mut message = if failures.len() == 1 {
            failures.into_iter().next().unwrap_or_default()
        } else {
            format!(
                "all {} configured providers failed -- {}",
                failures.len(),
                failures.join(" | ")
            )
        };
        if !skipped.is_empty() {
            message.push_str(&format!(" ({} skipped for want of image input)", skipped.join(", ")));
        }
        Err(AgentError::Llm(message))
    }

    fn name(&self) -> &str { &self.name }

    fn supports_image_input(&self) -> bool {
        self.clients.iter().any(|client| client.supports_image_input())
    }

    fn supports_image_generation(&self) -> bool {
        self.clients.iter().any(|client| client.supports_image_generation())
    }

    async fn generate_image(
        &self,
        prompt: &str,
        reference_images: &[(String, String)],
    ) -> Result<crate::llm_client::GeneratedImage, AgentError> {
        let mut last_error = None;
        for client in &self.clients {
            if !client.supports_image_generation() { continue; }
            match client.generate_image(prompt, reference_images).await {
                Ok(image) => return Ok(image),
                Err(error) => {
                    tracing::warn!(target: "ai_agent", provider = client.name(), "image provider failed; trying next configured provider");
                    last_error = Some(format!("{}: {error}", client.name()));
                }
            }
        }
        Err(AgentError::Llm(last_error.unwrap_or_else(|| "no image-generation provider is configured".into())))
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
/// Ordered backup providers used after the primary is unavailable or out of quota.
pub const ENV_FALLBACK_ORDER: &str = "AI_FALLBACK_ORDER";
/// Environment variable naming the Grok fallback model id (optional).
pub const ENV_GROK_MODEL: &str = "GROK_MODEL";
/// Environment variable carrying the Grok fallback API token (optional).
pub const ENV_GROK_API_TOKEN: &str = "GROK_API_TOKEN";
/// Accepted xAI spelling for the deployment Grok API token.
/// `XAI_API_KEY` is commonly used when provisioning xAI credentials.
pub const ENV_XAI_API_KEY: &str = "XAI_API_KEY";
/// Environment variable overriding Grok's default base URL (optional).
pub const ENV_GROK_BASE_URL: &str = "GROK_BASE_URL";
/// Environment variable carrying the Groq fallback API key.
pub const ENV_GROQ_API_KEY: &str = "GROQ_API_KEY";
/// Environment variable naming the Groq fallback model id (optional).
pub const ENV_GROQ_MODEL: &str = "GROQ_MODEL";

/// The providers tried, in order, when `AI_FALLBACK_ORDER` is unset.
///
/// Listing a provider here costs nothing when its credentials are absent --
/// `fallback_clients_from_env` drops it -- so the chain grows without the
/// operator having to set the variable to match.
pub const DEFAULT_FALLBACK_ORDER: &str = "cloudflare,gemini,grok,groq";

/// Build the configured Cloudflare, Gemini, Grok and Groq backups without exposing
/// their credentials. Missing credentials simply make that provider unavailable.
pub fn fallback_clients_from_env(
    exclude: Option<ProviderId>,
) -> Vec<std::sync::Arc<dyn crate::llm_client::LlmClient>> {
    let order = std::env::var(ENV_FALLBACK_ORDER)
        .unwrap_or_else(|_| DEFAULT_FALLBACK_ORDER.into());
    fallback_clients_from(&order, exclude, |name| std::env::var(name).ok())
}

fn fallback_clients_from(
    order: &str,
    exclude: Option<ProviderId>,
    var: impl Fn(&str) -> Option<String>,
) -> Vec<std::sync::Arc<dyn crate::llm_client::LlmClient>> {
    order.split(',').filter_map(|name| {
        let provider = ProviderId::from_name(name.trim())?;
        if Some(provider) == exclude { return None; }
        let (model, key, base_url) = match provider {
            ProviderId::Cloudflare => {
                let account = var("CLOUDFLARE_ACCOUNT_ID")?;
                let key = var("CLOUDFLARE_API_TOKEN")?;
                let model = var("CLOUDFLARE_AI_MODEL")
                    .unwrap_or_else(|| provider.default_model().into());
                let base = format!("https://api.cloudflare.com/client/v4/accounts/{}/ai/v1", account.trim());
                (model, key, base)
            }
            ProviderId::Gemini => {
                let key = var("GEMINI_API_KEY")?;
                let model = var("GEMINI_MODEL").unwrap_or_else(|| provider.default_model().into());
                (model, key, provider.default_base_url().into())
            }
            ProviderId::Grok => {
                let token = var(ENV_GROK_API_TOKEN).or_else(|| var(ENV_XAI_API_KEY))?;
                let model = var(ENV_GROK_MODEL)
                    .unwrap_or_else(|| provider.default_model().into());
                let base = var(ENV_GROK_BASE_URL)
                    .unwrap_or_else(|| provider.default_base_url().into());
                (model, token, base)
            }
            ProviderId::Groq => {
                let key = var(ENV_GROQ_API_KEY)?;
                let model = var(ENV_GROQ_MODEL)
                    .unwrap_or_else(|| provider.default_model().into());
                (model, key, provider.default_base_url().into())
            }
            _ => return None,
        };
        if key.trim().is_empty() || model.trim().is_empty() { return None; }
        let mut config = OpenAiCompatConfig::for_provider(provider, model.trim(), key.trim());
        config.base_url = base_url;
        let client = OpenAiCompatClient::new(config).ok()?;
        Some(std::sync::Arc::new(client) as std::sync::Arc<dyn crate::llm_client::LlmClient>)
    }).collect()
}

fn with_env_fallbacks(
    primary: std::sync::Arc<dyn crate::llm_client::LlmClient>,
    provider: ProviderId,
) -> std::sync::Arc<dyn crate::llm_client::LlmClient> {
    let mut clients = vec![primary];
    clients.extend(fallback_clients_from_env(Some(provider)));
    std::sync::Arc::new(FallbackClient::new(clients))
}

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
                 openai, anthropic, openrouter, deepseek, grok, groq, huggingface, gemini, cloudflare, openai-compat."
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
            Ok((with_env_fallbacks(std::sync::Arc::new(client), ProviderId::Bedrock), model))
        }
        provider @ (ProviderId::OpenAi
        | ProviderId::Anthropic
        | ProviderId::OpenRouter
        | ProviderId::DeepSeek
        | ProviderId::Grok
        | ProviderId::Groq
        | ProviderId::HuggingFace
        | ProviderId::Gemini
        | ProviderId::Cloudflare
        | ProviderId::OpenAiCompat) => {
            // `provider` already carries the matched id here -- a shadowing
            // re-bind would be a no-op, and current clippy flags exactly that.
            let model = match std::env::var(ENV_MODEL) {
                Ok(model) if !model.trim().is_empty() => model.trim().to_string(),
                _ => {
                    let provider_model_var = match provider {
                        ProviderId::Gemini => Some("GEMINI_MODEL"),
                        ProviderId::Cloudflare => Some("CLOUDFLARE_AI_MODEL"),
                        ProviderId::Groq => Some(ENV_GROQ_MODEL),
                        _ => None,
                    };
                    provider_model_var.and_then(|name| std::env::var(name).ok())
                        .filter(|model| !model.trim().is_empty())
                        .unwrap_or_else(|| provider.default_model().to_string())
                },
            };
            if model.is_empty() {
                return Err(AgentError::NotConfigured(format!(
                    "{ENV_MODEL} is not set and `{}` has no built-in default model. \
                     Set {ENV_MODEL} to the model id the endpoint expects.",
                    provider.wire_name()
                )));
            }

            let provider_key_var = match provider {
                ProviderId::Gemini => Some("GEMINI_API_KEY"),
                ProviderId::Cloudflare => Some("CLOUDFLARE_API_TOKEN"),
                ProviderId::Groq => Some(ENV_GROQ_API_KEY),
                _ => None,
            };
            let api_key = std::env::var(ENV_API_KEY)
                .ok()
                .or_else(|| provider_key_var.and_then(|name| std::env::var(name).ok()))
                .unwrap_or_default().trim().to_string();
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
            if provider == ProviderId::Cloudflare && config.base_url.is_empty() {
                if let Ok(account) = std::env::var("CLOUDFLARE_ACCOUNT_ID") {
                    config.base_url = format!(
                        "https://api.cloudflare.com/client/v4/accounts/{}/ai/v1",
                        account.trim()
                    );
                }
            }
            // A custom base URL frequently means a gateway doing its own auth
            // (a local vLLM, a sidecar): an empty key is legitimate there, so
            // the key requirement above is only enforced against the provider
            // default, not against an operator-chosen endpoint.
            if matches!(provider, ProviderId::OpenAiCompat | ProviderId::Cloudflare)
                && config.base_url.is_empty()
            {
                return Err(AgentError::NotConfigured(format!(
                    "{ENV_PROVIDER} is `{}`, which has no default endpoint. Set {ENV_BASE_URL} \
                     to its API root (Cloudflare requires the account id in the URL).",
                    provider.wire_name()
                )));
            }

            let model = config.model_id.clone();
            let client = OpenAiCompatClient::new(config)
                .map_err(|e| AgentError::NotConfigured(e.to_string()))?;
            Ok((with_env_fallbacks(std::sync::Arc::new(client), provider), model))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::{ContentBlock, LlmRequest, LlmResponse, Message, StopReason, Usage};
    use crate::llm_client::LlmClient as _;

    /// One rung of a chain, named, that either answers or refuses.
    struct Rung {
        label: &'static str,
        vision: bool,
        /// `Some(text)` answers with it; `None` fails the call.
        answer: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl crate::llm_client::LlmClient for Rung {
        async fn complete(&self, _request: LlmRequest) -> Result<LlmResponse, AgentError> {
            match self.answer {
                Some(text) => Ok(LlmResponse {
                    message: Message::assistant(text),
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
                None => Err(AgentError::Llm(format!("{} is out of quota", self.label))),
            }
        }
        fn supports_image_input(&self) -> bool { self.vision }
        fn name(&self) -> &str { self.label }
    }

    fn chain(rungs: Vec<Rung>) -> FallbackClient {
        FallbackClient::new(
            rungs.into_iter()
                .map(|rung| std::sync::Arc::new(rung) as std::sync::Arc<dyn crate::llm_client::LlmClient>)
                .collect(),
        )
    }

    fn text_request() -> LlmRequest {
        LlmRequest {
            system: None,
            messages: vec![Message::user("what is the POC?")],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: 512,
            temperature: 0.0,
        }
    }

    fn picture_request() -> LlmRequest {
        let mut request = text_request();
        request.messages.push(Message {
            role: crate::llm_client::Role::User,
            content: vec![
                ContentBlock::Text("read this chart".into()),
                ContentBlock::Image { media_type: "image/png".into(), data: "eA==".into() },
            ],
        });
        request
    }

    /// The whole point of the chain: a provider that fails is not the end of
    /// the run, it is the start of the next rung.
    #[tokio::test]
    async fn a_failing_provider_moves_the_request_to_the_next_one() {
        let client = chain(vec![
            Rung { label: "first", vision: true, answer: None },
            Rung { label: "second", vision: true, answer: Some("the POC is 103250") },
        ]);
        let response = client.complete(text_request()).await.expect("second rung answers");
        assert_eq!(response.text(), "the POC is 103250");
    }

    /// Every rung refusing names every rung. Before this the message carried
    /// only the last failure, so a three-provider chain reported "third is out
    /// of quota" and the operator had no way to see the other two had been
    /// tried -- which is exactly what makes a failover look absent.
    #[tokio::test]
    async fn a_chain_that_all_fails_names_every_rung_that_refused() {
        let client = chain(vec![
            Rung { label: "first", vision: true, answer: None },
            Rung { label: "second", vision: true, answer: None },
        ]);
        let err = client.complete(text_request()).await.expect_err("nothing answered");
        let message = err.to_string();
        assert!(message.contains("first"), "{message}");
        assert!(message.contains("second"), "{message}");
        assert!(message.contains("all 2 configured providers failed"), "{message}");
    }

    /// A picture cannot be handed to a text-only rung. It is skipped, and the
    /// refusal says so rather than blaming the providers that were tried.
    #[tokio::test]
    async fn a_picture_skips_a_rung_that_cannot_read_one_and_says_so() {
        let client = chain(vec![
            Rung { label: "blind", vision: false, answer: Some("never reached") },
            Rung { label: "sighted", vision: true, answer: None },
        ]);
        let err = client.complete(picture_request()).await.expect_err("sighted refused");
        let message = err.to_string();
        assert!(message.contains("sighted is out of quota"), "{message}");
        assert!(!message.contains("never reached"), "{message}");
        assert!(message.contains("skipped for want of image input"), "{message}");
        assert!(message.contains("blind"), "{message}");
    }

    /// A picture with no rung that can read it is a configuration problem, and
    /// the answer has to name the fix, not the exception.
    #[tokio::test]
    async fn a_picture_with_no_vision_rung_says_what_to_configure() {
        let client = chain(vec![
            Rung { label: "blind(a)", vision: false, answer: Some("never reached") },
        ]);
        let err = client.complete(picture_request()).await.expect_err("no eyes anywhere");
        let message = err.to_string();
        assert!(message.contains("cannot process image input"), "{message}");
        assert!(message.contains("blind(a)"), "{message}");
        assert!(message.contains("vision-capable"), "{message}");
    }

    /// One rung alone keeps its own message -- the chain must not invent a
    /// count for a provider that never had a neighbour.
    #[tokio::test]
    async fn a_single_provider_reports_its_own_failure_plainly() {
        let client = chain(vec![Rung { label: "lonely", vision: true, answer: None }]);
        let err = client.complete(text_request()).await.expect_err("one rung, refusing");
        let message = err.to_string();
        assert!(message.contains("lonely"), "{message}");
        assert!(!message.contains("configured providers failed"), "{message}");
        assert!(!message.contains("skipped"), "{message}");
    }

    /// An empty chain is not a silent success.
    #[tokio::test]
    async fn an_empty_chain_says_nothing_is_configured() {
        let client = FallbackClient::new(Vec::new());
        let err = client.complete(text_request()).await.expect_err("no rungs at all");
        assert!(err.to_string().contains("no AI provider is configured"), "{err}");
    }

    #[test]
    fn wire_names_round_trip() {
        for provider in [
            ProviderId::Bedrock,
            ProviderId::OpenAi,
            ProviderId::Anthropic,
            ProviderId::OpenRouter,
            ProviderId::DeepSeek,
            ProviderId::Grok,
            ProviderId::Groq,
            ProviderId::HuggingFace,
            ProviderId::Gemini,
            ProviderId::Cloudflare,
            ProviderId::OpenAiCompat,
        ] {
            assert_eq!(ProviderId::from_name(provider.wire_name()), Some(provider));
        }
    }

    #[test]
    fn aliases_and_casing_are_accepted() {
        assert_eq!(ProviderId::from_name("xai"), Some(ProviderId::Grok));
        assert_eq!(ProviderId::from_name("X.AI"), Some(ProviderId::Grok));
        assert_eq!(ProviderId::from_name("groq"), Some(ProviderId::Groq));
        assert_eq!(ProviderId::from_name("GroqCloud"), Some(ProviderId::Groq));
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
            ProviderId::Groq,
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

    /// A Grok token turns Grok into a real rung of the chain: it is not the
    /// only way to reach an LLM, it is the third one. Before this the builder
    /// matched Cloudflare and Gemini and dropped everything else, so a
    /// deployment that named `grok` in `AI_FALLBACK_ORDER` got no client and
    /// no warning.
    #[test]
    fn a_configured_grok_token_makes_grok_a_fallback() {
        let clients = fallback_clients_from(
            "cloudflare,gemini,grok",
            None,
            |name| (name == ENV_GROK_API_TOKEN).then(|| "k".into()),
        );
        assert_eq!(clients.len(), 1, "only Grok has credentials here");
        assert!(
            clients[0].name().starts_with("grok("),
            "{}",
            clients[0].name()
        );
    }

    #[test]
    fn xai_api_key_alias_configures_grok_fallback() {
        let clients = fallback_clients_from(
            "cloudflare,gemini,grok",
            None,
            |name| (name == ENV_XAI_API_KEY).then(|| "xai-test-key".into()),
        );
        assert_eq!(clients.len(), 1);
        assert!(clients[0].name().starts_with("grok("), "{}", clients[0].name());
    }

    #[test]
    fn configured_groq_key_builds_a_vision_capable_fallback() {
        let clients = fallback_clients_from(
            "groq",
            None,
            |name| (name == ENV_GROQ_API_KEY).then(|| "groq-test-key".into()),
        );
        assert_eq!(clients.len(), 1);
        assert!(clients[0].name().starts_with("groq("), "{}", clients[0].name());
        assert!(clients[0].supports_image_input());

        let with_model = fallback_clients_from("groq", None, |name| match name {
            ENV_GROQ_API_KEY => Some("groq-test-key".into()),
            ENV_GROQ_MODEL => Some("qwen/qwen3.8-27b".into()),
            _ => None,
        });
        assert!(with_model[0].supports_image_input());
    }

    /// Grok's model and endpoint are overridable, and default to the
    /// provider's own when the operator has no opinion.
    #[test]
    fn grok_defaults_to_xai_and_honours_its_overrides() {
        let with_defaults = fallback_clients_from(
            "grok",
            None,
            |name| (name == ENV_GROK_API_TOKEN).then(|| "k".into()),
        );
        assert_eq!(with_defaults[0].name(), format!("grok({})", ProviderId::Grok.default_model()));

        let with_model = fallback_clients_from("grok", None, |name| match name {
            ENV_GROK_API_TOKEN => Some("k".into()),
            ENV_GROK_MODEL => Some("grok-4.7".into()),
            _ => None,
        });
        assert_eq!(with_model[0].name(), "grok(grok-4.7)");
        assert!(
            with_model[0].supports_image_input(),
            "a Grok 4.x rung has to be usable by the chart reviews that ask for pictures"
        );
    }

    /// The old text-only Grok is not a vision rung: a run that photographs
    /// charts would otherwise hand six pictures to a model that cannot read
    /// them and get a provider error back for every one.
    #[test]
    fn a_text_only_grok_is_not_counted_as_a_vision_rung() {
        let clients = fallback_clients_from(
            "grok",
            None,
            |name| match name {
                ENV_GROK_API_TOKEN => Some("k".into()),
                ENV_GROK_MODEL => Some("grok-3".into()),
                _ => None,
            },
        );
        assert!(!clients[0].supports_image_input(), "grok-3 reads no pictures");
    }

    /// Missing credentials are not an error: the provider is absent from the
    /// chain and everything else still builds. This is what makes providers
    /// safe to list in the default order before deployment keys are configured.
    #[test]
    fn a_fallback_without_credentials_is_dropped_not_fatal() {
        let clients = fallback_clients_from(DEFAULT_FALLBACK_ORDER, None, |_| None);
        assert!(clients.is_empty(), "no configured provider has a key");

        let only_gemini = fallback_clients_from(
            DEFAULT_FALLBACK_ORDER,
            None,
            |name| (name == "GEMINI_API_KEY").then(|| "k".into()),
        );
        assert_eq!(only_gemini.len(), 1);
        assert!(only_gemini[0].name().starts_with("gemini("));
    }

    /// A provider is never its own backup: the primary is excluded so a
    /// quota error does not retry the identical call.
    #[test]
    fn the_primary_provider_is_not_its_own_fallback() {
        let baseline = |name: &str| {
            matches!(
                name,
                "GEMINI_API_KEY"
                    | "CLOUDFLARE_ACCOUNT_ID"
                    | "CLOUDFLARE_API_TOKEN"
                    | ENV_GROQ_API_KEY
            )
            .then(|| "k".into())
        };
        let with_grok = |name: &str| {
            if name == ENV_GROK_API_TOKEN { Some("k".into()) } else { baseline(name) }
        };
        assert_eq!(fallback_clients_from(DEFAULT_FALLBACK_ORDER, None, with_grok).len(), 4);
        let clients = fallback_clients_from(DEFAULT_FALLBACK_ORDER, Some(ProviderId::Grok), with_grok);
        assert_eq!(clients.len(), 3, "grok excluded from its own chain");
        assert!(clients.iter().all(|c| !c.name().starts_with("grok(")));
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
