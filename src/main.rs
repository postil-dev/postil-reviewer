use std::{path::PathBuf, process::ExitCode};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::{Parser, Subcommand};
use postil_reviewer::config::{
    self, RuntimeConfig, RuntimeFileConfig, RuntimeOverrides, Severity, resolve_target,
};
use postil_reviewer::github::{CheckOutput, GithubClient, check_conclusion};
use postil_reviewer::openrouter::{self, OpenRouterClient};
use postil_reviewer::review::{
    ReviewEnvelope, TokenUsage, append_status, apply_config, parse_envelope, status_line,
};

#[derive(Debug, Parser)]
#[command(name = "postil", about = "Postil review bot")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Review(ReviewArgs),
}

#[derive(Debug, Parser, Default)]
struct ReviewArgs {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    repo: Option<String>,
    #[arg(long)]
    pr: Option<u64>,
    #[arg(long)]
    sha: Option<String>,
    #[arg(long, value_enum)]
    fail_on: Option<Severity>,
    #[arg(long)]
    no_inline: bool,
    #[arg(long)]
    github_token: Option<String>,
    #[arg(long)]
    openrouter_api_key: Option<String>,
    #[arg(long)]
    review_model: Option<String>,
    #[arg(long)]
    review_model_cascade: Option<String>,
    #[arg(long)]
    github_api_url: Option<String>,
    #[arg(long)]
    openrouter_api_url: Option<String>,
    #[arg(long)]
    diff_limit: Option<usize>,
    #[arg(long)]
    check_name: Option<String>,
    #[arg(long)]
    output_json: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli
        .command
        .unwrap_or(Command::Review(ReviewArgs::default()))
    {
        Command::Review(args) => run_review(args).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("[postil] {err:#}");
            ExitCode::from(1)
        }
    }
}

async fn run_review(args: ReviewArgs) -> Result<ExitCode> {
    let output_json = args.output_json.clone();
    let file_cfg = match args.config.as_ref() {
        Some(path) => Some(RuntimeFileConfig::from_path(path)?),
        None => None,
    };
    let overrides = RuntimeOverrides {
        github_token: args.github_token,
        openrouter_api_key: args.openrouter_api_key,
        review_model: args.review_model,
        review_model_cascade: args.review_model_cascade,
        github_api_url: args.github_api_url,
        openrouter_api_url: args.openrouter_api_url,
        fail_on: args.fail_on,
        no_inline: args.no_inline.then_some(true),
        diff_limit: args.diff_limit,
        check_name: args.check_name,
        repo: args.repo,
        pr: args.pr,
        sha: args.sha,
        ..RuntimeOverrides::default()
    };
    let runtime = RuntimeConfig::load(file_cfg, overrides)?;
    let target = resolve_target(&runtime)?;
    let github = GithubClient::new(runtime.github_api_url.clone(), runtime.github_token.clone())?;
    let openrouter = OpenRouterClient::new(
        runtime.openrouter_api_url.clone(),
        runtime.openrouter_api_key.clone(),
    )?;
    let started_at = Utc::now().to_rfc3339();

    let diff = github.fetch_diff(&target, runtime.diff_limit).await?;
    if diff.trim().is_empty() {
        println!("[postil] empty diff - nothing to review");
        if let Some(path) = output_json.as_ref() {
            write_envelope(
                path,
                &ReviewEnvelope {
                    summary: "Nothing to review.".to_string(),
                    findings: Vec::new(),
                    usage: TokenUsage::default(),
                    model_used: "none".to_string(),
                },
            )?;
        }
        github
            .create_check_run(
                &target,
                &runtime.check_name,
                "neutral",
                CheckOutput::empty(),
                &started_at,
            )
            .await?;
        return Ok(ExitCode::SUCCESS);
    }

    let repo_config = match &runtime.file_review_config {
        Some(config) => config.clone(),
        None => github.load_repo_config(&target).await?,
    };
    if !repo_config.enabled {
        let output = CheckOutput {
            title: "Postil Review".to_string(),
            summary: "Postil is disabled for this repo via config.".to_string(),
            text: None,
        };
        github
            .create_check_run(&target, &runtime.check_name, "neutral", output, &started_at)
            .await?;
        if let Some(path) = output_json.as_ref() {
            write_envelope(
                path,
                &ReviewEnvelope {
                    summary: "Postil is disabled for this repo via config.".to_string(),
                    findings: Vec::new(),
                    usage: TokenUsage::default(),
                    model_used: "none".to_string(),
                },
            )?;
        }
        println!(
            "[postil] {}/{}#{} -> neutral (disabled by config)",
            target.owner, target.repo, target.pull_number
        );
        return Ok(ExitCode::SUCCESS);
    }

    let model_result = run_cascade(&openrouter, &runtime.models(), &diff).await?;
    let envelope = apply_config(
        parse_envelope(
            &model_result.content,
            model_result.usage,
            model_result.model_used,
        ),
        &repo_config,
    )?;
    if let Some(path) = output_json {
        write_envelope(&path, &envelope)?;
    }
    let inline_count = envelope.findings.len();
    let clean = envelope.findings.is_empty();
    let should_post = repo_config.review.enabled
        && !(clean && repo_config.review.on_clean == config::OnClean::Skip);
    let label = if clean { "clean" } else { "needs-attention" };
    let review_body = append_status(
        if envelope.summary.trim().is_empty() {
            "Postil reviewed this PR."
        } else {
            &envelope.summary
        },
        &status_line(&envelope, inline_count, label),
    );

    if should_post && !runtime.no_inline {
        if let Err(err) = github
            .post_inline_review(&target, &envelope, &review_body)
            .await
        {
            eprintln!("[postil] inline review post failed: {err:#}");
            github
                .post_issue_comment(&target, &review_body)
                .await
                .context("post fallback issue comment after review failure")?;
        }
    }

    let conclusion = check_conclusion(&envelope);
    github
        .create_check_run(
            &target,
            &runtime.check_name,
            conclusion,
            CheckOutput::from_envelope(&envelope),
            &started_at,
        )
        .await?;
    println!(
        "[postil] {}/{}#{} -> {} ({} findings, model: {})",
        target.owner,
        target.repo,
        target.pull_number,
        conclusion,
        envelope.findings.len(),
        envelope.model_used
    );

    if envelope
        .findings
        .iter()
        .any(|f| f.severity.rank() >= runtime.fail_on.rank())
    {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn write_envelope(path: &std::path::Path, envelope: &ReviewEnvelope) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(envelope)?)
        .with_context(|| format!("write {}", path.display()))
}

async fn run_cascade(
    client: &OpenRouterClient,
    models: &[String],
    diff: &str,
) -> Result<openrouter::OpenRouterResult> {
    let mut errors = Vec::new();
    for model in models {
        match client.complete(model, diff).await {
            Ok(result) => return Ok(result),
            Err(err) => errors.push(format!("{model}: {err:#}")),
        }
    }
    Err(anyhow!("all models failed:\n{}", errors.join("\n")))
}
