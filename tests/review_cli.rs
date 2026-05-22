use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::tempdir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

#[tokio::test]
async fn posts_review_check_and_json_output() {
    let github = MockServer::start().await;
    let openrouter = MockServer::start().await;
    let dir = tempdir().unwrap();
    let config = dir.path().join("postil.yaml");
    let output = dir.path().join("result.json");
    fs::write(
        &config,
        format!(
            r#"
githubToken: test-github
openrouterApiKey: test-openrouter
githubApiUrl: {}
openrouterApiUrl: {}
repo: owner/repo
pr: 42
sha: abc123
reviewModel: xiaomi/mimo-v2.5-pro
failOn: error
review:
  severityThreshold: info
  maxFindings: 25
  review:
    enabled: true
"#,
            github.uri(),
            openrouter.uri()
        ),
    )
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/42"))
        .and(header("accept", "application/vnd.github.v3.diff"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("diff --git a/src/lib.rs b/src/lib.rs\n+let x = 1;"),
        )
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"summary\":\"Needs work.\",\"findings\":[{\"path\":\"src/lib.rs\",\"line\":1,\"severity\":\"warn\",\"body\":\"This can fail.\"}]}"}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        })))
        .mount(&openrouter)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .mount(&github)
        .await;

    Command::cargo_bin("postil")
        .unwrap()
        .args([
            "review",
            "--config",
            config.to_str().unwrap(),
            "--output-json",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("neutral (1 findings"));

    let result: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output).unwrap()).unwrap();
    assert_eq!(result["modelUsed"], "xiaomi/mimo-v2.5-pro");
    assert_eq!(result["findings"][0]["severity"], "warn");
}

#[tokio::test]
async fn exits_nonzero_when_fail_on_matches() {
    let github = MockServer::start().await;
    let openrouter = MockServer::start().await;
    let dir = tempdir().unwrap();
    let config = dir.path().join("postil.yaml");
    fs::write(
        &config,
        format!(
            r#"
githubToken: test-github
openrouterApiKey: test-openrouter
githubApiUrl: {}
openrouterApiUrl: {}
repo: owner/repo
pr: 42
sha: abc123
reviewModel: xiaomi/mimo-v2.5-pro
failOn: warn
noInline: true
review:
  review:
    enabled: true
"#,
            github.uri(),
            openrouter.uri()
        ),
    )
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_string("diff\n+bug"))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"summary\":\"Needs work.\",\"findings\":[{\"path\":\"src/lib.rs\",\"line\":1,\"severity\":\"warn\",\"body\":\"This can fail.\"}]}"} }]
        })))
        .mount(&openrouter)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .mount(&github)
        .await;

    Command::cargo_bin("postil")
        .unwrap()
        .args(["review", "--config", config.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("neutral (1 findings"));
}

#[tokio::test]
async fn cascades_models_after_provider_failure() {
    let github = MockServer::start().await;
    let openrouter = MockServer::start().await;
    let dir = tempdir().unwrap();
    let config = dir.path().join("postil.yaml");
    fs::write(
        &config,
        format!(
            r#"
githubToken: test-github
openrouterApiKey: test-openrouter
githubApiUrl: {}
openrouterApiUrl: {}
repo: owner/repo
pr: 42
sha: abc123
reviewModelCascade: bad/model, xiaomi/mimo-v2.5-pro
noInline: true
review:
  review:
    enabled: false
"#,
            github.uri(),
            openrouter.uri()
        ),
    )
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_string("diff\n+ok"))
        .mount(&github)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("down"))
        .up_to_n_times(1)
        .mount(&openrouter)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "{\"summary\":\"Clean.\",\"findings\":[]}"} }]
        })))
        .mount(&openrouter)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/check-runs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .mount(&github)
        .await;

    Command::cargo_bin("postil")
        .unwrap()
        .args(["review", "--config", config.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("model: xiaomi/mimo-v2.5-pro"));
}
