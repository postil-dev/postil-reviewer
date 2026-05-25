use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::review::TokenUsage;

#[derive(Debug, Clone)]
pub struct OpenRouterClient {
    http: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, Clone)]
pub struct OpenRouterResult {
    pub content: String,
    pub usage: TokenUsage,
    pub model_used: String,
}

impl OpenRouterClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("build OpenRouter HTTP client")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    pub async fn complete(
        &self,
        model: &str,
        system_prompt: &str,
        user_content: &str,
    ) -> Result<OpenRouterResult> {
        let url = format!("{}/chat/completions", self.base_url);
        let res = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .header("http-referer", "https://postil.dev")
            .header("x-title", "Postil")
            .json(&CompletionRequest::new(model, system_prompt, user_content))
            .send()
            .await
            .context("send OpenRouter request")?;
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(anyhow!(
                "openrouter {}: {}",
                status.as_u16(),
                body.chars().take(400).collect::<String>()
            ));
        }
        let data: CompletionResponse = res.json().await.context("parse OpenRouter response")?;
        Ok(OpenRouterResult {
            content: data
                .choices
                .first()
                .and_then(|c| c.message.content.clone())
                .unwrap_or_default(),
            usage: TokenUsage {
                prompt_tokens: data
                    .usage
                    .as_ref()
                    .and_then(|u| u.prompt_tokens)
                    .unwrap_or(0),
                completion_tokens: data
                    .usage
                    .as_ref()
                    .and_then(|u| u.completion_tokens)
                    .unwrap_or(0),
                total_tokens: data
                    .usage
                    .as_ref()
                    .and_then(|u| u.total_tokens)
                    .unwrap_or(0),
            },
            model_used: model.to_string(),
        })
    }
}

#[derive(Debug, Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    max_tokens: u32,
    response_format: ResponseFormat,
}

impl<'a> CompletionRequest<'a> {
    fn new(model: &'a str, system_prompt: &'a str, user_content: &'a str) -> Self {
        Self {
            model,
            messages: vec![
                Message {
                    role: "system",
                    content: system_prompt,
                },
                Message {
                    role: "user",
                    content: user_content,
                },
            ],
            temperature: 0.2,
            max_tokens: 2500,
            response_format: ResponseFormat {
                kind: "json_object",
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: MessageResponse,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}
