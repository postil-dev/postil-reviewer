# Postil Reviewer

`postil` is the review-bot binary for Postil. Hosted Postil workers and the
GitHub Action both run this CLI; the website does not contain review logic.

## Usage

```bash
postil review --repo owner/repo --pr 123 --sha HEAD_SHA
```

By default, the command also reads GitHub Actions context:

- `GITHUB_REPOSITORY`
- `GITHUB_EVENT_PATH`
- `GITHUB_TOKEN`
- `OPENROUTER_API_KEY`

Review models are configured with `REVIEW_MODEL` or `REVIEW_MODEL_CASCADE`.
The default model is `moonshotai/kimi-k2.6`.

## Configuration

Runtime configuration is env-first, with full CLI and file parity. Precedence is:

1. CLI flags
2. Environment variables
3. `--config` file
4. Built-in defaults

Example runtime config:

```yaml
repo: owner/repo
pr: 123
sha: abc123
reviewModel: xiaomi/mimo-v2.5-pro
failOn: error
noInline: false
review:
  enabled: true
  ignore:
    - "dist/**"
  severityThreshold: info
  maxFindings: 25
  review:
    enabled: true
    onClean: approve
```

The CLI also loads per-repository review config from the pull request head SHA,
using this order:

1. `.postil.yaml`, `.postil.yml`, `.postil.json`
2. `.coderabbit.yaml`, `.coderabbit.yml`
3. `.kodo.yaml`, `.kodo.yml`
4. Built-in defaults

For compatibility, `requiredChecks` and `autoMergeTimeoutMs` are accepted both
top-level and under `review`.

## JSON Output

Hosted callers can request the structured review envelope:

```bash
postil review --output-json .cache/postil-review.json
```

The JSON includes `summary`, `findings`, `usage`, and `modelUsed`.

## Testing

```bash
cargo test --quiet
```

Live OpenRouter smoke test:

```bash
infisical run --env=prod -- env REVIEW_MODEL=xiaomi/mimo-v2.5-pro cargo test --quiet live_openrouter_smoke -- --ignored
```
