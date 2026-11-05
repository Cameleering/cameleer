# Webhook component

> The `webhook` component receives events from a SaaS service through a webhook: it serves an HTTP endpoint for a delegate endpoint (GitHub or Telegram) and registers that endpoint with the service on start.

**Scheme:** `webhook:` · **Crate:** `cameleer-saas` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Webhook](https://camel.apache.org/components/next/webhook-component.html)

`webhook:` wraps another endpoint URI. On start it binds an HTTP path on
[platform-http](platform-http.md) and asks the service to call
`webhookExternalUrl + path`; on stop it removes the registration. Delegates that support
webhooks: `github` (repository hooks) and `telegram` (`setWebhook`).

## When to use it

- Start an AI code-review agent when a pull request is opened, without polling GitHub.
- Answer Telegram bot messages as they arrive.
- Any push-based integration where the service needs a public URL to call.

## URI format

```text
webhook:delegateScheme:delegatePath[?options]
```

Options starting with `webhook` belong to this component; all other options are passed to the
delegate endpoint.

## Example

Pull request events from a GitHub repository feed a review queue. On start Cameleer creates
the repository hook pointing at `PUBLIC_URL/github`; on stop it deletes it.

<!-- example: webhook-github-pull-requests -->
```yaml
- route:
    id: pr-intake
    from:
      uri: "webhook:github:pullRequest?repoOwner={{env:GITHUB_OWNER:acme}}&repoName={{env:GITHUB_REPO:shop}}&oauthToken={{env:GITHUB_TOKEN:changeme}}&serverUrl={{env:GITHUB_API:https://api.github.com}}&webhookExternalUrl={{env:PUBLIC_URL:https://hooks.example.com}}&webhookPath=/github"
      steps:
        - filter:
            simple: "${header.GitHubEvent} == 'pull_request' && ${body[action]} == 'opened'"
            steps:
              - setBody:
                  simple: "Review pull request #${body[number]}"
              - to: seda:review-queue
- route:
    id: reviewer
    from:
      uri: seda:review-queue
      steps:
        - log: "${body}"
```

The `reviewer` route is where an agent would pick up the work (for example with the `openai`
component).

## Rust DSL

```rust
fn pr_intake_routes(b: &mut RouteBuilder, github: &str, public_url: &str) {
    b.from(&format!("webhook:{}&webhookExternalUrl={}&webhookPath=/github", github, public_url))
        .route_id("pr-intake")
        .filter(simple("${header.GitHubEvent} == 'pull_request' && ${body[action]} == 'opened'"))
        .set_body(simple("Review pull request #${body[number]}"))
        .to("seda:review-queue");
    b.from("seda:review-queue").route_id("reviewer").log("${body}");
}
```

## Options

| option | default | description |
|---|---|---|
| `webhookExternalUrl` | | Public base URL announced to the service. Required when registering. Also from `camel.component.webhook.webhookExternalUrl`. |
| `webhookPath` | `/webhook/<scheme>-<hash>` | Path of the HTTP endpoint. |
| `webhookBasePath` | | Prefix for the path. |
| `webhookAutoRegister` | `true` | Register on start and unregister on stop. With `false`, register the URL yourself. |
| `webhookComponentName` | `platform-http` | HTTP server component; its port is `camel.server.port`. |

Delegate options are those of the delegate component, e.g. for `github`: `repoOwner`,
`repoName`, `oauthToken`, `hookEvents` (default `push,pull_request,issues`) and `serverUrl` (API
base URL, default `https://api.github.com`, for GitHub Enterprise); for `telegram`:
`authorizationToken` and `baseUri`.

## Message headers

| header | direction | description |
|---|---|---|
| `GitHubEvent` | out | GitHub: the `X-GitHub-Event` request header (`pull_request`, `push`, `issues`). |
| `CamelTelegramChatId` | out | Telegram: the chat of the incoming message. |

The body is the parsed JSON payload (GitHub) or the Telegram message.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e GITHUB_OWNER -e GITHUB_REPO -e GITHUB_TOKEN \
  -e PUBLIC_URL=https://hooks.example.com -v "$PWD:/routes:ro" \
  pmsudhi/cameleer run /routes/pr-intake.camel.yaml
```

`PUBLIC_URL` must reach port 8080 of the container (a reverse proxy or a tunnel).

## Common questions

### Why does start fail with "webhookExternalUrl"?
Registration needs the public URL. Set `webhookExternalUrl`, or `webhookAutoRegister=false` if
you register the hook yourself.

### Is the GitHub payload signature checked?
No. Cameleer does not verify `X-Hub-Signature-256`. Keep the path secret, put a proxy that
checks the signature in front, or restrict the source addresses.

### Which port does the webhook listen on?
The `platform-http` port, `camel.server.port` (default 8080).

## Related

- [platform-http](platform-http.md), [http](http.md), [rest](rest.md).
- Apache Camel: [Webhook component](https://camel.apache.org/components/next/webhook-component.html).
