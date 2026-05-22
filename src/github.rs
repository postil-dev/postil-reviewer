use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    config::{RepoReviewConfig, ReviewTarget, translate_coderabbit, translate_kodo},
    review::{Finding, ReviewEnvelope},
};

#[derive(Debug, Clone)]
pub struct GithubClient {
    http: Client,
    base_url: String,
    token: String,
}

impl GithubClient {
    pub fn new(base_url: String, token: String) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .user_agent("postil-reviewer/0.1.0")
                .build()
                .context("build GitHub HTTP client")?,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    pub async fn fetch_diff(&self, target: &ReviewTarget, limit: usize) -> Result<String> {
        let res = self
            .http
            .get(self.pull_url(target))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github.v3.diff")
            .send()
            .await
            .context("fetch pull request diff")?;
        let status = res.status();
        if !status.is_success() {
            return Err(anyhow!(
                "github diff {}: {}",
                status.as_u16(),
                res.text().await.unwrap_or_default()
            ));
        }
        let diff = res.text().await.context("read diff body")?;
        Ok(if diff.len() > limit {
            format!("{}\n\n[diff truncated]", &diff[..limit])
        } else {
            diff
        })
    }

    pub async fn load_repo_config(&self, target: &ReviewTarget) -> Result<RepoReviewConfig> {
        let candidates = [
            (".postil.yaml", "postil"),
            (".postil.yml", "postil"),
            (".postil.json", "postil"),
            (".coderabbit.yaml", "coderabbit"),
            (".coderabbit.yml", "coderabbit"),
            (".kodo.yaml", "kodo"),
            (".kodo.yml", "kodo"),
        ];
        let Some(ref sha) = target.head_sha else {
            return Ok(RepoReviewConfig::default());
        };
        for (path, kind) in candidates {
            match self.fetch_raw_file(target, sha, path).await {
                Ok(Some(text)) => {
                    let parsed = match kind {
                        "postil" => RepoReviewConfig::from_text(path, &text),
                        "coderabbit" => translate_coderabbit(&text),
                        "kodo" => translate_kodo(&text),
                        _ => unreachable!(),
                    };
                    if let Ok(config) = parsed {
                        return Ok(config);
                    }
                }
                Ok(None) => {}
                Err(err) => return Err(err),
            }
        }
        Ok(RepoReviewConfig::default())
    }

    async fn fetch_raw_file(
        &self,
        target: &ReviewTarget,
        sha: &str,
        path: &str,
    ) -> Result<Option<String>> {
        let url = format!(
            "{}/repos/{}/{}/contents/{}?ref={}",
            self.base_url, target.owner, target.repo, path, sha
        );
        let res = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github.v3.raw")
            .send()
            .await
            .with_context(|| format!("fetch config {path}"))?;
        if res.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = res.status();
        if !status.is_success() {
            return Err(anyhow!(
                "github config {path} {}: {}",
                status.as_u16(),
                res.text().await.unwrap_or_default()
            ));
        }
        Ok(Some(res.text().await.context("read config body")?))
    }

    pub async fn post_inline_review(
        &self,
        target: &ReviewTarget,
        envelope: &ReviewEnvelope,
        body: &str,
    ) -> Result<()> {
        let comments: Vec<ReviewComment> = envelope
            .findings
            .iter()
            .map(|f| ReviewComment {
                path: f.path.clone(),
                line: f.line,
                side: "RIGHT",
                body: format!("**{}** · {}", f.severity.as_str().to_uppercase(), f.body),
            })
            .collect();
        let payload = PullReviewRequest {
            commit_id: target.head_sha.clone(),
            event: if comments.is_empty() {
                "APPROVE"
            } else {
                "COMMENT"
            },
            body,
            comments: if comments.is_empty() {
                None
            } else {
                Some(comments)
            },
        };
        let res = self
            .http
            .post(format!(
                "{}/repos/{}/{}/pulls/{}/reviews",
                self.base_url, target.owner, target.repo, target.pull_number
            ))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("post pull request review")?;
        let status = res.status();
        if !status.is_success() {
            return Err(anyhow!(
                "github review {}: {}",
                status.as_u16(),
                res.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    pub async fn post_issue_comment(&self, target: &ReviewTarget, body: &str) -> Result<()> {
        let res = self
            .http
            .post(format!(
                "{}/repos/{}/{}/issues/{}/comments",
                self.base_url, target.owner, target.repo, target.pull_number
            ))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .json(&json!({ "body": body }))
            .send()
            .await
            .context("post fallback issue comment")?;
        let status = res.status();
        if !status.is_success() {
            return Err(anyhow!(
                "github issue comment {}: {}",
                status.as_u16(),
                res.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    pub async fn create_check_run(
        &self,
        target: &ReviewTarget,
        check_name: &str,
        conclusion: &str,
        output: CheckOutput,
        started_at: &str,
    ) -> Result<()> {
        let Some(head_sha) = target.head_sha.as_deref() else {
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let payload = json!({
            "name": check_name,
            "head_sha": head_sha,
            "status": "completed",
            "conclusion": conclusion,
            "started_at": started_at,
            "completed_at": now,
            "output": output,
        });
        let res = self
            .http
            .post(format!(
                "{}/repos/{}/{}/check-runs",
                self.base_url, target.owner, target.repo
            ))
            .bearer_auth(&self.token)
            .header("accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("create check run")?;
        let status = res.status();
        if !status.is_success() {
            return Err(anyhow!(
                "github check-run {}: {}",
                status.as_u16(),
                res.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    fn pull_url(&self, target: &ReviewTarget) -> String {
        format!(
            "{}/repos/{}/{}/pulls/{}",
            self.base_url, target.owner, target.repo, target.pull_number
        )
    }
}

#[derive(Debug, Serialize)]
struct PullReviewRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_id: Option<String>,
    event: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<Vec<ReviewComment>>,
}

#[derive(Debug, Serialize)]
struct ReviewComment {
    path: String,
    line: u64,
    side: &'static str,
    body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutput {
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl CheckOutput {
    pub fn empty() -> Self {
        Self {
            title: "Empty diff".to_string(),
            summary: "Nothing to review.".to_string(),
            text: None,
        }
    }

    pub fn from_envelope(envelope: &ReviewEnvelope) -> Self {
        let mut errors = 0;
        let mut warnings = 0;
        for finding in &envelope.findings {
            match finding.severity {
                crate::config::Severity::Error => errors += 1,
                crate::config::Severity::Warn => warnings += 1,
                crate::config::Severity::Info => {}
            }
        }
        let title = if errors > 0 {
            format!("{errors} error{}", if errors == 1 { "" } else { "s" })
        } else if warnings > 0 {
            format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" })
        } else {
            "No issues".to_string()
        };
        let text = if envelope.findings.is_empty() {
            Some("No issues found.".to_string())
        } else {
            Some(render_findings(&envelope.findings))
        };
        Self {
            title,
            summary: if envelope.summary.trim().is_empty() {
                if envelope.findings.is_empty() {
                    "No issues found.".to_string()
                } else {
                    "See inline review comments.".to_string()
                }
            } else {
                envelope.summary.clone()
            },
            text,
        }
    }
}

fn render_findings(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| {
            format!(
                "**{}** `{}`:{}\n\n{}",
                f.severity.as_str().to_uppercase(),
                f.path,
                f.line,
                f.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

pub fn check_conclusion(envelope: &ReviewEnvelope) -> &'static str {
    if envelope
        .findings
        .iter()
        .any(|f| f.severity == crate::config::Severity::Error)
    {
        "failure"
    } else if envelope
        .findings
        .iter()
        .any(|f| f.severity == crate::config::Severity::Warn)
    {
        "neutral"
    } else {
        "success"
    }
}
