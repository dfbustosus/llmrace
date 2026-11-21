//! Provider registry: identities, endpoints, defaults and availability
//! for every supported provider.

use crate::error::{Error, Result};
use std::sync::Arc;

/// Streaming wire protocol spoken by a provider endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI `/v1/chat/completions` streaming (also Groq, OpenRouter, Ollama, vLLM…).
    OpenAiCompat,
    /// Anthropic `/v1/messages` streaming.
    Anthropic,
    /// Google Gemini `streamGenerateContent` streaming.
    Gemini,
    /// Cohere `/v2/chat` streaming.
    Cohere,
    /// Amazon Bedrock `converse-stream` (binary event-stream).
    Bedrock,
    /// Google Vertex AI `streamGenerateContent` (SSE, Gemini shape).
    Vertex,
    /// Built-in deterministic provider for tests and demos.
    Mock,
}

/// How the API key is presented to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    /// `Authorization: Bearer <key>` (OpenAI-compatible default).
    #[default]
    Bearer,
    /// `api-key: <key>` (Azure OpenAI).
    ApiKeyHeader,
}

/// A fully-resolved provider description.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    /// Stable lowercase identifier used on the CLI (e.g. `openai`).
    pub id: Arc<str>,
    /// Name shown in the TUI and reports.
    pub display_name: Arc<str>,
    /// Absolute endpoint URL.
    pub endpoint: Arc<str>,
    /// Environment variable that holds the API key, if any.
    pub api_key_env: Option<Arc<str>>,
    /// Model used when `--model` is not given.
    pub default_model: Arc<str>,
    /// Wire protocol of the endpoint.
    pub wire: Wire,
    /// Authentication style for the API key.
    pub auth: AuthKind,
    /// Opt-in local daemons are excluded from env-var auto-detect: they
    /// would otherwise join every bare `llmrace` run with an error lane
    /// whenever the daemon is not running.
    pub opt_in: bool,
    /// Experimental providers: implemented and vector-tested, but the
    /// end-to-end path is not yet live-verified against the real service.
    pub experimental: bool,
}

impl ProviderSpec {
    /// OpenAI official API.
    pub fn openai() -> Self {
        spec(
            "openai",
            "https://api.openai.com/v1/chat/completions",
            Some("OPENAI_API_KEY"),
            "gpt-4o-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Anthropic official API.
    pub fn anthropic() -> Self {
        spec(
            "anthropic",
            "https://api.anthropic.com/v1/messages",
            Some("ANTHROPIC_API_KEY"),
            "claude-3-5-haiku-latest",
            Wire::Anthropic,
        )
    }

    /// Groq ultra-low-latency inference.
    pub fn groq() -> Self {
        spec(
            "groq",
            "https://api.groq.com/openai/v1/chat/completions",
            Some("GROQ_API_KEY"),
            "llama-3.3-70b-versatile",
            Wire::OpenAiCompat,
        )
    }

    /// OpenRouter multi-provider gateway.
    pub fn openrouter() -> Self {
        spec(
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
            Some("OPENROUTER_API_KEY"),
            "openai/gpt-4o-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Google Gemini via `streamGenerateContent`.
    pub fn gemini() -> Self {
        spec(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse",
            Some("GEMINI_API_KEY"),
            "gemini-2.5-flash",
            Wire::Gemini,
        )
    }

    /// xAI Grok official API.
    pub fn xai() -> Self {
        spec(
            "xai",
            "https://api.x.ai/v1/chat/completions",
            Some("XAI_API_KEY"),
            "grok-3-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Perplexity Sonar API.
    pub fn perplexity() -> Self {
        spec(
            "perplexity",
            "https://api.perplexity.ai/chat/completions",
            Some("PERPLEXITY_API_KEY"),
            "sonar",
            Wire::OpenAiCompat,
        )
    }

    /// Cohere official API (v2 chat streaming).
    pub fn cohere() -> Self {
        spec(
            "cohere",
            "https://api.cohere.com/v2/chat",
            Some("CO_API_KEY"),
            "command-a-03-2025",
            Wire::Cohere,
        )
    }

    /// DeepSeek official API.
    pub fn deepseek() -> Self {
        spec(
            "deepseek",
            "https://api.deepseek.com/v1/chat/completions",
            Some("DEEPSEEK_API_KEY"),
            "deepseek-chat",
            Wire::OpenAiCompat,
        )
    }

    /// Mistral official API.
    pub fn mistral() -> Self {
        spec(
            "mistral",
            "https://api.mistral.ai/v1/chat/completions",
            Some("MISTRAL_API_KEY"),
            "mistral-small-latest",
            Wire::OpenAiCompat,
        )
    }

    /// Together inference cloud.
    pub fn together() -> Self {
        spec(
            "together",
            "https://api.together.xyz/v1/chat/completions",
            Some("TOGETHER_API_KEY"),
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            Wire::OpenAiCompat,
        )
    }

    /// Fireworks inference cloud.
    pub fn fireworks() -> Self {
        spec(
            "fireworks",
            "https://api.fireworks.ai/inference/v1/chat/completions",
            Some("FIREWORKS_API_KEY"),
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
            Wire::OpenAiCompat,
        )
    }

    /// Cerebras wafer-scale inference.
    pub fn cerebras() -> Self {
        spec(
            "cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            Some("CEREBRAS_API_KEY"),
            "llama-3.3-70b",
            Wire::OpenAiCompat,
        )
    }

    /// Local Ollama daemon.
    pub fn ollama() -> Self {
        spec(
            "ollama",
            "http://localhost:11434/v1/chat/completions",
            None,
            "llama3.2",
            Wire::OpenAiCompat,
        )
    }

    /// DeepInfra inference cloud.
    pub fn deepinfra() -> Self {
        spec(
            "deepinfra",
            "https://api.deepinfra.com/v1/openai/chat/completions",
            Some("DEEPINFRA_API_KEY"),
            "meta-llama/Meta-Llama-3.1-8B-Instruct",
            Wire::OpenAiCompat,
        )
    }

    /// Hyperbolic inference cloud.
    pub fn hyperbolic() -> Self {
        spec(
            "hyperbolic",
            "https://api.hyperbolic.xyz/v1/chat/completions",
            Some("HYPERBOLIC_API_KEY"),
            "meta-llama/Meta-Llama-3.1-70B-Instruct",
            Wire::OpenAiCompat,
        )
    }

    /// Local LM Studio server (default port 1234).
    pub fn lmstudio() -> Self {
        let mut p = spec(
            "lmstudio",
            "http://localhost:1234/v1/chat/completions",
            None,
            "openai/gpt-4o-mini",
            Wire::OpenAiCompat,
        );
        p.opt_in = true;
        p
    }

    /// Local llama.cpp `llama-server` (default port 8080).
    pub fn llamacpp() -> Self {
        let mut p = spec(
            "llamacpp",
            "http://localhost:8080/v1/chat/completions",
            None,
            "local-model",
            Wire::OpenAiCompat,
        );
        p.opt_in = true;
        p
    }

    /// Amazon Bedrock Converse Stream.
    ///
    /// **Experimental**: the SigV4 signer and binary event-stream decoder
    /// are unit-tested against published AWS vectors, and the request body
    /// is the model-agnostic Converse document, but the end-to-end path
    /// has not been verified against a live Bedrock account — see issue
    /// #126. Credentials: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`
    /// (optional `AWS_SESSION_TOKEN`) and `AWS_REGION`.
    pub fn bedrock() -> Self {
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let endpoint = format!(
            "https://bedrock-runtime.{region}.amazonaws.com/model/{{model}}/converse-stream"
        );
        let mut p = spec(
            "bedrock",
            &endpoint,
            Some("AWS_ACCESS_KEY_ID"),
            "anthropic.claude-3-5-haiku-20241022-v1:0",
            Wire::Bedrock,
        );
        p.experimental = true;
        p
    }

    /// Google Vertex AI (service-account OAuth2, Gemini wire).
    ///
    /// **Experimental**: the JWT construction and RS256 signing are
    /// structurally tested, but the token exchange and streaming path have
    /// not been verified against a live GCP project — see issue #127.
    /// Credentials: `GOOGLE_APPLICATION_CREDENTIALS` (service-account
    /// JSON), `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_REGION`.
    pub fn vertex() -> Self {
        let project =
            std::env::var("GOOGLE_CLOUD_PROJECT").unwrap_or_else(|_| "YOUR_PROJECT".to_string());
        let region =
            std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".to_string());
        let endpoint = format!(
            "https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/google/models/{{model}}:streamGenerateContent?alt=sse",
        );
        let mut p = spec(
            "vertex",
            &endpoint,
            Some("GOOGLE_APPLICATION_CREDENTIALS"),
            "gemini-2.5-flash",
            Wire::Vertex,
        );
        p.experimental = true;
        p
    }

    /// Deterministic in-process provider for tests and demos.
    pub fn mock() -> Self {
        spec("mock", "", None, "mock-1", Wire::Mock)
    }

    /// Builds a custom provider against any OpenAI-compatible endpoint
    /// (vLLM, LiteLLM proxy, Together, Fireworks…).
    pub fn custom(name: &str, endpoint: &str, key_env: Option<&str>, model: &str) -> Self {
        spec(name, endpoint, key_env, model, Wire::OpenAiCompat)
    }

    /// Builds an Azure OpenAI provider for a resource + deployment.
    ///
    /// Reads the key from `AZURE_OPENAI_API_KEY` and authenticates with the
    /// `api-key` header instead of a bearer token.
    pub fn azure_openai(resource: &str, deployment: &str) -> Self {
        let endpoint = format!(
            "https://{resource}.openai.azure.com/openai/deployments/{deployment}/chat/completions?api-version=2024-08-01-preview"
        );
        let mut p = spec(
            "azure",
            &endpoint,
            Some("AZURE_OPENAI_API_KEY"),
            deployment,
            Wire::OpenAiCompat,
        );
        p.auth = AuthKind::ApiKeyHeader;
        p
    }

    /// All built-in providers.
    pub fn registry() -> Vec<ProviderSpec> {
        vec![
            ProviderSpec::openai(),
            ProviderSpec::anthropic(),
            ProviderSpec::gemini(),
            ProviderSpec::groq(),
            ProviderSpec::xai(),
            ProviderSpec::perplexity(),
            ProviderSpec::cohere(),
            ProviderSpec::deepseek(),
            ProviderSpec::mistral(),
            ProviderSpec::together(),
            ProviderSpec::fireworks(),
            ProviderSpec::cerebras(),
            ProviderSpec::deepinfra(),
            ProviderSpec::hyperbolic(),
            ProviderSpec::openrouter(),
            ProviderSpec::ollama(),
            ProviderSpec::bedrock(),
            ProviderSpec::vertex(),
            ProviderSpec::lmstudio(),
            ProviderSpec::llamacpp(),
            ProviderSpec::mock(),
        ]
    }

    /// Looks up a built-in provider by CLI name.
    pub fn find(name: &str) -> Option<ProviderSpec> {
        ProviderSpec::registry()
            .into_iter()
            .find(|p| *p.id == *name)
    }

    /// `true` when the required API key is present in the environment.
    pub fn is_available(&self) -> bool {
        self.api_key_env
            .as_ref()
            .map(|env| std::env::var(env.as_ref()).is_ok())
            .unwrap_or(true)
    }

    pub(super) fn api_key(&self) -> Result<Option<String>> {
        match &self.api_key_env {
            Some(env) => std::env::var(env.as_ref())
                .ok()
                .map(Some)
                .ok_or_else(|| Error::Config(format!("missing env var {}", env))),
            None => Ok(None),
        }
    }
}

fn spec(id: &str, endpoint: &str, key_env: Option<&str>, model: &str, wire: Wire) -> ProviderSpec {
    ProviderSpec {
        id: Arc::from(id),
        display_name: Arc::from(id),
        endpoint: Arc::from(endpoint),
        api_key_env: key_env.map(Arc::from),
        default_model: Arc::from(model),
        wire,
        auth: AuthKind::default(),
        opt_in: false,
        experimental: false,
    }
}
