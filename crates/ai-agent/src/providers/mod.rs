//! Concrete [`LlmClient`](crate::llm_client::LlmClient) implementations.

pub mod bedrock;

pub use bedrock::{BedrockClient, BedrockConfig};
