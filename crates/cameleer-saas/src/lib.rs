//! # cameleer-saas
//!
//! SaaS / API connectors over REST, std only (HTTPS through `cameleer-http` / `cameleer-tls`):
//!
//! | scheme | module | Camel component |
//! |---|---|---|
//! | `slack` | [`slack`] | camel-slack (webhook + `chat.postMessage` producer, `conversations.history` consumer) |
//! | `telegram` | [`telegram`] | camel-telegram (`sendMessage`/`sendPhoto`... producer, `getUpdates` consumer, webhook) |
//! | `github` | [`github`] | camel-github (pull request / issue / commit / tag consumers, issue, comment and status producers, webhook) |
//! | `twilio` | [`twilio`] | camel-twilio (`message/create`, `message/fetch`, `message/read`) |
//! | `jira` | [`jira`] | camel-jira (`addIssue`, `addComment`, `deleteIssue`, `fetchIssue`, `searchIssues`, `newIssues` consumer) |
//! | `openai` | [`openai`] | OpenAI-compatible chat completions and embeddings (cf. `langchain4j-chat`) |
//! | `webhook` | [`webhook`] | camel-webhook (exposes webhook-capable endpoints over `platform-http`) |
//! | `graphql` | [`graphql`] | camel-graphql |
//! | `rss`, `atom` | [`feed`] | camel-rss / camel-atom polling consumers |
//!
//! Every connector has an endpoint-override option (`serverUrl`, `baseUri`, `baseUrl`,
//! `jiraUrl`, `webhookUrl`...) and an embedded mock server in [`testing`], so tests never
//! reach the internet. `weather` is not provided.

pub mod feed;
pub mod github;
pub mod graphql;
pub mod jira;
pub mod openai;
pub mod rest;
pub mod slack;
pub mod telegram;
pub mod testing;
pub mod twilio;
pub mod webhook;

use std::sync::Arc;

use cameleer_core::context::CamelContext;

/// Register the connectors on a context (lazily created): `slack`, `telegram`, `github`,
/// `twilio`, `jira`, `openai`, `webhook`, `graphql`, `rss`, `atom`. The `webhook` component
/// needs an HTTP consumer component (`platform-http`, see `cameleer_http::register`).
pub fn register(ctx: &CamelContext) {
    rest::register_kinds();
    ctx.add_component_factory("slack", || Arc::new(slack::SlackComponent));
    ctx.add_component_factory("telegram", || Arc::new(telegram::TelegramComponent));
    ctx.add_component_factory("github", || Arc::new(github::GithubComponent));
    ctx.add_component_factory("twilio", || Arc::new(twilio::TwilioComponent));
    ctx.add_component_factory("jira", || Arc::new(jira::JiraComponent));
    ctx.add_component_factory("openai", || Arc::new(openai::OpenAiComponent));
    ctx.add_component_factory("webhook", || Arc::new(webhook::WebhookComponent));
    ctx.add_component_factory("graphql", || Arc::new(graphql::GraphqlComponent));
    ctx.add_component_factory("rss", || Arc::new(feed::FeedComponent::rss()));
    ctx.add_component_factory("atom", || Arc::new(feed::FeedComponent::atom()));
}
