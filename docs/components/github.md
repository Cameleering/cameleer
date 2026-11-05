# GitHub component

> The `github` component polls a GitHub repository for new pull requests, issues, commits, tags and events, and creates issues, comments and commit statuses through the GitHub REST API.

**Scheme:** `github:` · **Crate:** `cameleer-saas` ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [GitHub](https://camel.apache.org/components/next/github-component.html)

## When to use it

- Greet or triage new pull requests and issues automatically (labels, first comment, review bot).
- Let an AI agent open an issue when it finds a bug, a failed job or a security finding.
- Feed new commits or tags into a release, deployment or changelog pipeline.
- Read the files a pull request changes, for example to hand them to an LLM reviewer.

## URI format

```text
github:type[/branch]?repoOwner=owner&repoName=repo[&options]
```

Consumer types: `pullRequest`, `issue`, `commit/<branch>`, `tag`, `event`.
Producer types: `createIssue`, `pullRequestComment`, `issueComment`, `pullRequestState`,
`pullRequestFiles`, `getCommitFile`, `closePullRequest`.

## Example

Comment every new pull request. The consumer sets the `GitHubPullRequest` header to the pull
request number, so the producer knows where to comment. The first poll only records the pull
requests that are already open.

<!-- example: github-pr-welcome -->
```yaml
- route:
    id: pr-welcome
    from:
      uri: "github:pullRequest?repoOwner={{env:GITHUB_OWNER:apache}}&repoName={{env:GITHUB_REPO:camel}}&oauthToken={{env:GITHUB_TOKEN}}&serverUrl={{env:GITHUB_API_URL:https://api.github.com}}&delay=30000"
      steps:
        - log: "new pull request #${header.GitHubPullRequest}: ${body[title]}"
        - setBody:
            simple: "Thanks for the pull request '${body[title]}'. A maintainer will review it soon."
        - to: "github:pullRequestComment?repoOwner={{env:GITHUB_OWNER:apache}}&repoName={{env:GITHUB_REPO:camel}}&oauthToken={{env:GITHUB_TOKEN}}&serverUrl={{env:GITHUB_API_URL:https://api.github.com}}"
```

An agent that files an issue. Other routes (a failed nightly job, an LLM log analyser) send the
problem description to `direct:file-bug`; the title comes from the `GitHubIssueTitle` header and
the body becomes the issue text. Afterwards the body is the created issue and `GitHubIssue` its
number.

<!-- example: github-agent-issue -->
```yaml
- route:
    id: file-bug
    from:
      uri: "direct:file-bug"
      steps:
        - setHeader:
            name: GitHubIssueTitle
            simple: "Nightly job failed: ${header.job}"
        - to: "github:createIssue?repoOwner={{env:GITHUB_OWNER:apache}}&repoName={{env:GITHUB_REPO:camel}}&oauthToken={{env:GITHUB_TOKEN}}&serverUrl={{env:GITHUB_API_URL:https://api.github.com}}"
        - log: "opened issue #${header.GitHubIssue}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("github:pullRequest?repoOwner=apache&repoName=camel&oauthToken={{env:GITHUB_TOKEN}}&delay=30000")
        .set_body(simple("Thanks for the pull request '${body[title]}'. A maintainer will review it soon."))
        .to("github:pullRequestComment?repoOwner=apache&repoName=camel&oauthToken={{env:GITHUB_TOKEN}}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `repoOwner` | | Repository owner (required). |
| `repoName` | | Repository name (required). |
| `oauthToken` | | Personal access token or GitHub App token, sent as a Bearer token. |
| `startingSha` | `last` | `commit` consumer: `last` skips existing commits, `beginning` delivers them all, oldest first. |
| `state` | | `pullRequestState`: `success`, `error`, `failure` or `pending` (required there). |
| `targetUrl` | | `pullRequestState`: link shown with the status. |
| `encoding` | | `getCommitFile`: `base64` keeps the content base64-encoded instead of decoding it. |
| `hookEvents` | `push,pull_request,issues` | Events subscribed when used through `webhook:github:...`. |
| `serverUrl` | `https://api.github.com` | API base URL (Cameleer extension: GitHub Enterprise `https://ghe.example.com/api/v3`, a test server). |
| `delay`, `initialDelay`, ... | `delay=10000` | Consumers: the standard scheduled-poll options. |

`eventFetchStrategy` is accepted for Camel compatibility and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `GitHubPullRequest` | in / out | Pull request number: set by the `pullRequest` consumer; read by `pullRequestComment`, `pullRequestFiles`, `closePullRequest`. |
| `GitHubIssue` | in / out | Issue number: set by the `issue` consumer and `createIssue`; read by `issueComment`. |
| `GitHubIssueTitle` | in | `createIssue`: the title (required). |
| `GitHubInResponseTo` | in | `pullRequestComment`: reply to this review comment id. |
| `GitHubPullRequestHeadCommitSHA` | in | `pullRequestState`: commit to set the status on. |
| `GitHubCommitSha`, `GitHubCommitAuthor`, `GitHubCommitCommitter`, `GitHubCommitUrl` | out | `commit` consumer (the body is the commit message). |
| `GitHubEvent`, `GitHubEventId`, `CamelGitHubEventPayload` | out | `event` consumer (the body is the event type, such as `PushEvent`). Webhooks set `GitHubEvent` from `X-GitHub-Event`. |

## Run it with Docker

```sh
export GITHUB_TOKEN=ghp_... GITHUB_OWNER=my-org GITHUB_REPO=my-repo
docker run --rm -e GITHUB_TOKEN -e GITHUB_OWNER -e GITHUB_REPO -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/pr-welcome.camel.yaml
```

## Common questions

### How do I get the files of a pull request for an AI review?

Send an exchange with the `GitHubPullRequest` header to `github:pullRequestFiles`: the body
becomes the list of changed files (`filename`, `status`, `additions`, `sha`, ...). Split it and
send each file to `github:getCommitFile` to get the decoded content, then to an LLM.

### Can I receive webhooks instead of polling?

Yes: consume from `webhook:github:pullRequest?repoOwner=...&repoName=...&oauthToken=...&webhookExternalUrl=https://ci.example.com`.
The repository hook is created on start and deleted on stop; the payload becomes the body.

### Does the issue consumer return pull requests?

No. GitHub lists pull requests as issues, but the `issue` consumer filters them out. Use
`pullRequest` for those.

### How do I set a CI status on a commit?

Use `github:pullRequestState?state=success&targetUrl=https://ci.example.com/run/42` with the
`GitHubPullRequestHeadCommitSHA` header; the body becomes the status description.

## Related

- [jira](jira.md), [slack](slack.md)
- [openai](openai.md) and the [AI components guide](../guides/ai-components.md)
- [Apache Camel GitHub component](https://camel.apache.org/components/next/github-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
