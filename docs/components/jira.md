# Jira component

> The `jira` component creates, updates, transitions, comments and searches Jira issues over the Jira REST API v2, and polls for new issues, new comments and field changes.

**Scheme:** `jira:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [Jira](https://camel.apache.org/components/next/jira-component.html)

## When to use it

- Open a ticket automatically when monitoring, a failed job or an AI agent detects a problem.
- Forward new issues or comments to Slack, Teams or e-mail.
- React to status or priority changes (escalate when an issue becomes `Blocker`).
- Attach logs, add work logs, link related issues from an integration flow.

## URI format

```text
jira:type?jiraUrl=https://your-site.atlassian.net&username=...&password=...[&options]
```

Producer types: `addIssue`, `updateIssue`, `addComment`, `deleteIssue`, `fetchIssue`,
`fetchComments`, `searchIssues`, `transitionIssue`, `attach`, `watchers`, `addIssueLink`,
`addWorkLog`. Consumer types: `newIssues`, `newComments`, `watchUpdates`.

## Example

An agent that turns an incident into a ticket. The body becomes the issue description; project,
type and summary come from headers. After the call the body is the created issue (`id`, `key`,
`self`) and the `IssueKey` header holds its key.

<!-- example: jira-agent-ticket -->
```yaml
- route:
    id: incident-to-jira
    from:
      uri: "direct:incident"
      steps:
        - setHeader:
            name: ProjectKey
            constant: "OPS"
        - setHeader:
            name: IssueTypeName
            constant: "Bug"
        - setHeader:
            name: IssueSummary
            simple: "Incident on ${header.service}"
        - setHeader:
            name: IssuePriorityName
            constant: "High"
        - to: "jira:addIssue?jiraUrl={{env:JIRA_URL:https://your-site.atlassian.net}}&username={{env:JIRA_USER}}&password={{env:JIRA_API_TOKEN}}"
        - log: "created ${header.IssueKey}"
```

Announce every new issue of a project in Slack. The consumer's first poll records the issues
that already exist; after that each new issue is delivered once, with the `IssueKey` header.

<!-- example: jira-new-issues-to-slack -->
```yaml
- route:
    id: new-issues-to-slack
    from:
      uri: "jira:newIssues?jiraUrl={{env:JIRA_URL:https://your-site.atlassian.net}}&username={{env:JIRA_USER}}&password={{env:JIRA_API_TOKEN}}&jql=project=OPS&delay=60000"
      steps:
        - setBody:
            simple: "New Jira issue ${header.IssueKey}: ${body[fields][summary]}"
        - to: "slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:incident")
        .set_header("ProjectKey", constant("OPS"))
        .set_header("IssueTypeName", constant("Bug"))
        .set_header("IssueSummary", simple("Incident on ${header.service}"))
        .to("jira:addIssue?jiraUrl={{env:JIRA_URL:https://your-site.atlassian.net}}&username={{env:JIRA_USER}}&password={{env:JIRA_API_TOKEN}}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `jiraUrl` | | Base URL of the Jira site (required). |
| `username` | | User (e-mail on Jira Cloud) for Basic authentication. |
| `password` | | Password or API token for Basic authentication. |
| `accessToken` | | Personal access token (Bearer). Used instead of `username`/`password` when set. |
| `jql` | | JQL query: required by the consumers, optional for `searchIssues`. `ORDER BY key desc` is appended when the query has no `ORDER BY`. |
| `maxResults` | `50` | Maximum issues per search. |
| `watchedFields` | `Status,Priority` | `watchUpdates`: fields to watch (case-insensitive). |
| `sendOnlyUpdatedField` | `true` | `watchUpdates`: send only the new field value (`false` sends the whole issue). |
| `delay`, `initialDelay`, ... | `delay=6000` | Consumers: the standard scheduled-poll options. |

`consumerKey`, `privateKey` and `verificationCode` (OAuth 1.0a) are accepted for Camel
compatibility but not used: authenticate with an API token or a personal access token.

## Message headers

| header | direction | description |
|---|---|---|
| `ProjectKey` | in | `addIssue`: project key (required). |
| `IssueTypeName` / `IssueTypeId` | in | `addIssue` (one required), `updateIssue`. |
| `IssueSummary` | in | `addIssue` (required), `updateIssue`. |
| `IssueAssignee` / `IssueAssigneeId` | in | Assignee name or account id. |
| `IssuePriorityName` / `IssuePriorityId` | in | Priority. |
| `IssueComponents` | in | Comma-separated component names. |
| `IssueKey` | in / out | Target issue of most producers; set by `addIssue` and by the consumers. |
| `CamelJiraJql` | in | `searchIssues`: JQL (else the `jql` option, else the body). |
| `IssueTransitionId` | in | `transitionIssue`: transition id; a non-null body is added as a comment. |
| `IssueWatchersAdd` / `IssueWatchersRemove` | in | `watchers`: comma-separated text or a list of user names. |
| `ParentIssueKey`, `ChildIssueKey`, `linkType` | in | `addIssueLink`. |
| `minutesSpent` | in | `addWorkLog`; the body is the work log comment. |
| `CamelFileName` | in | `attach`: file name (default `attachment`); the body is the content. |
| `IssueChanged`, `IssueWatchedIssues` | out | `watchUpdates`: the changed field and the keys of the watched issues. |

## Run it with Docker

```sh
export JIRA_URL=https://your-site.atlassian.net JIRA_USER=bot@example.com JIRA_API_TOKEN=... SLACK_WEBHOOK_URL=...
docker run --rm -e JIRA_URL -e JIRA_USER -e JIRA_API_TOKEN -e SLACK_WEBHOOK_URL \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/new-issues-to-slack.camel.yaml
```

## Common questions

### How do I authenticate with Jira Cloud?

Use your account e-mail as `username` and an API token (from id.atlassian.com) as `password`.
For Jira Data Center, a personal access token in `accessToken` works too.

### How do I let an LLM write the ticket?

Call [openai](openai.md) first with a system message such as "Write a one-line Jira summary",
store the answer in the `IssueSummary` header (`setHeader` with `${body}`), restore the original
text as the body, then call `jira:addIssue`. The [AI components guide](../guides/ai-components.md)
shows the pattern.

### How do I search issues?

`jira:searchIssues?jql=project=OPS AND status=Open` (or the JQL in the body or the
`CamelJiraJql` header); the body becomes the list of issues.

### Does it use Jira REST API v3?

No, v2 (`/rest/api/2`), which Jira Cloud and Data Center both still serve. Descriptions and
comments are plain text, not Atlassian Document Format.

## Related

- [github](github.md), [slack](slack.md), [openai](openai.md)
- [Apache Camel Jira component](https://camel.apache.org/components/next/jira-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
