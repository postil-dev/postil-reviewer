use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{RepoReviewConfig, Severity};

pub const SYSTEM_PROMPT: &str = r#"You are Postil, a code reviewer. You receive a unified diff for a pull request
and produce structured findings as JSON. Rules:
- Focus on correctness, security, and obvious bugs.
- Do not flag style, formatting, imports, or naming unless they cause a bug.
- Every finding cites a specific path and line number that exists in the diff.
- Severity is one of: info, warn, error.
- If the diff looks fine, return an empty findings array and a short summary.

Reply with ONLY a single JSON object, no prose, no markdown fence:
{
  "summary": "<2-4 sentences max summarizing overall risk posture. Do NOT restate individual findings.>",
  "findings": [ { "path": "...", "line": <int>, "severity": "info|warn|error", "body": "..." } ]
}"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub path: String,
    pub line: u64,
    pub severity: Severity,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEnvelope {
    pub summary: String,
    pub findings: Vec<Finding>,
    pub usage: TokenUsage,
    pub model_used: String,
}

pub fn parse_envelope(text: &str, usage: TokenUsage, model_used: String) -> ReviewEnvelope {
    let raw = fenced_json(text).unwrap_or(text);
    if let Some(slice) = json_object_slice(raw) {
        if let Ok(value) = serde_json::from_str::<Value>(slice) {
            let summary = value
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let findings = value
                .get("findings")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value::<Finding>(item.clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            return ReviewEnvelope {
                summary,
                findings,
                usage,
                model_used,
            };
        }
    }
    ReviewEnvelope {
        summary: text.trim().chars().take(4000).collect(),
        findings: Vec::new(),
        usage,
        model_used,
    }
}

fn fenced_json(text: &str) -> Option<&str> {
    let fence_start = text.find("```")?;
    let after_start = &text[fence_start + 3..];
    let content_start = after_start.find('\n').map(|i| i + 1).unwrap_or(0);
    let content = &after_start[content_start..];
    let fence_end = content.find("```")?;
    Some(content[..fence_end].trim())
}

fn json_object_slice(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

pub fn apply_config(
    mut envelope: ReviewEnvelope,
    cfg: &RepoReviewConfig,
) -> Result<ReviewEnvelope> {
    let mut set = GlobSetBuilder::new();
    for glob in &cfg.ignore {
        set.add(Glob::new(glob).with_context(|| format!("invalid ignore glob {glob}"))?);
    }
    let ignore = set.build().context("build ignore glob set")?;
    envelope.findings = envelope
        .findings
        .into_iter()
        .filter(|f| f.severity.rank() >= cfg.severity_threshold.rank())
        .filter(|f| !ignore.is_match(&f.path))
        .take(cfg.max_findings)
        .collect();
    Ok(envelope)
}

pub fn status_line(envelope: &ReviewEnvelope, inline_comments: usize, label: &str) -> String {
    let mut errors = 0;
    let mut warnings = 0;
    let mut infos = 0;
    for finding in &envelope.findings {
        match finding.severity {
            Severity::Error => errors += 1,
            Severity::Warn => warnings += 1,
            Severity::Info => infos += 1,
        }
    }
    format!(
        "Postil status: {label} | errors={errors} warnings={warnings} info={infos} inline_comments={inline_comments}"
    )
}

pub fn append_status(body: &str, status: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        status.to_string()
    } else {
        format!("{trimmed}\n\n{status}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json() {
        let env = parse_envelope(
            "```json\n{\"summary\":\"ok\",\"findings\":[{\"path\":\"src/lib.rs\",\"line\":3,\"severity\":\"warn\",\"body\":\"bug\"}]}\n```",
            TokenUsage::default(),
            "m".into(),
        );
        assert_eq!(env.summary, "ok");
        assert_eq!(env.findings.len(), 1);
        assert_eq!(env.findings[0].severity, Severity::Warn);
    }

    #[test]
    fn falls_back_to_summary() {
        let env = parse_envelope("plain output", TokenUsage::default(), "m".into());
        assert_eq!(env.summary, "plain output");
        assert!(env.findings.is_empty());
    }

    #[test]
    fn filters_findings() {
        let envelope = ReviewEnvelope {
            summary: "s".into(),
            usage: TokenUsage::default(),
            model_used: "m".into(),
            findings: vec![
                Finding {
                    path: "src/a.rs".into(),
                    line: 1,
                    severity: Severity::Info,
                    body: "i".into(),
                },
                Finding {
                    path: "dist/a.js".into(),
                    line: 1,
                    severity: Severity::Error,
                    body: "e".into(),
                },
                Finding {
                    path: "src/b.rs".into(),
                    line: 1,
                    severity: Severity::Warn,
                    body: "w".into(),
                },
            ],
        };
        let cfg = RepoReviewConfig {
            severity_threshold: Severity::Warn,
            ignore: vec!["dist/**".into()],
            ..RepoReviewConfig::default()
        };
        let filtered = apply_config(envelope, &cfg).unwrap();
        assert_eq!(filtered.findings.len(), 1);
        assert_eq!(filtered.findings[0].path, "src/b.rs");
    }
}
