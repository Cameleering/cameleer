//! Provider presets, request quirks and prompt caching of the model components, against the
//! provider mocks.
#![cfg(feature = "full")]

use cameleer_core::prelude::*;
use cameleer_saas::testing::{AnthropicMock, OpenAiMock};

fn context() -> CamelContext {
    cameleer_main::new_main().context().clone()
}

fn last_body(server: &cameleer_saas::testing::MockServer, path: &str) -> Value {
    server.requests_to(path).last().expect("a request").json()
}

#[test]
fn provider_presets_and_max_completion_tokens() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let base = format!("apiKey={}&baseUrl={}&maxTokens=50", OpenAiMock::API_KEY, ai.base_url());
    let gpt5 = format!("openai:chat-completion?provider=openai&model=gpt-5-mini&{}", base);
    let groq = format!("llm:chat-completion?provider=groq&model=llama-3.3-70b&{}", base);
    ctx.add_routes(move |b| {
        b.from("direct:gpt5").to(gpt5.as_str());
        b.from("direct:groq").to(groq.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:gpt5", Exchange::with_body(Some(ctx.clone()), "hi"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    let body = last_body(&ai.server, "/v1/chat/completions");
    // OpenAI reasoning models take max_completion_tokens
    assert_eq!(body.get("max_completion_tokens").and_then(|v| v.as_i64()), Some(50), "{}", body.to_json());
    assert!(body.get("max_tokens").is_none());
    let out = t.send("direct:groq", Exchange::with_body(Some(ctx.clone()), "hi"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    let body = last_body(&ai.server, "/v1/chat/completions");
    assert_eq!(body.get("max_tokens").and_then(|v| v.as_i64()), Some(50), "{}", body.to_json());
    assert!(body.get("max_completion_tokens").is_none());
    ctx.stop().unwrap();
    // an unknown provider is refused when the endpoint is created, and lists the presets
    let err = ctx.get_endpoint("openai:chat-completion?provider=nope&model=m").err().expect("refused");
    assert!(err.message.contains("unknown provider nope") && err.message.contains("groq") && err.message.contains("ollama"), "{}", err.message);
}

#[test]
fn anthropic_prompt_caching_marks_the_stable_prefix() {
    let m = AnthropicMock::start();
    let ctx = context();
    let base = format!("anthropic:messages?model=claude-test&apiKey={}&baseUrl={}&systemMessage=terse", AnthropicMock::API_KEY, m.base_url());
    let off = format!("{}&promptCaching=off", base);
    let tagged = format!("{}&tags=orders", base);
    ctx.add_routes(move |b| {
        b.from("ai-tool:lookup_order?description=Look up an order&tags=orders&parameter.orderId=string&readOnlyHint=true").set_body(simple("order ${header.orderId} is shipped"));
        b.from("direct:on").to(base.as_str());
        b.from("direct:off").to(off.as_str());
        b.from("direct:loop").to(tagged.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:on", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Echo: hello [terse]");
    assert_eq!(out.header("CamelAiCacheReadTokens").and_then(|v| v.as_i64()), Some(7));
    let body = last_body(&m.server, "/v1/messages");
    let system = body.get("system").unwrap();
    assert_eq!(system.to_json(), r#"[{"type":"text","text":"terse","cache_control":{"type":"ephemeral"}}]"#);
    let last = body.get("messages").and_then(|x| x.as_array()).and_then(|x| x.last().cloned()).unwrap();
    assert!(last.to_json().contains(r#""cache_control":{"type":"ephemeral"}"#), "{}", last.to_json());
    // tools are cached too: the breakpoint is on the last tool
    let out = t.send("direct:loop", Exchange::with_body(Some(ctx.clone()), r#"call lookup_order {"orderId":"A1"}"#));
    assert_eq!(out.body().to_string(), "Done: order A1 is shipped", "{:?}", out.exception);
    assert_eq!(out.header("CamelAiCacheReadTokens").and_then(|v| v.as_i64()), Some(14), "two turns of 7 cached tokens");
    let body = last_body(&m.server, "/v1/messages");
    let tools = body.get("tools").and_then(|x| x.as_array()).cloned().unwrap();
    assert!(tools.last().unwrap().get("cache_control").is_some());
    // promptCaching=off sends the request unchanged
    let out = t.send("direct:off", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert!(out.header("CamelAiCacheReadTokens").is_none());
    let body = last_body(&m.server, "/v1/messages");
    assert!(!body.to_json().contains("cache_control"), "{}", body.to_json());
    assert_eq!(body.get("system").unwrap().to_string(), "terse");
    ctx.stop().unwrap();
}

#[test]
fn fallback_models_and_the_response_cache() {
    let ai = OpenAiMock::start();
    let claude = AnthropicMock::start();
    let ctx = context();
    // the fallback is an endpoint URI; its own & are written as %26
    let fallback = format!("anthropic:messages?model=claude-test%26apiKey={}%26baseUrl={}", AnthropicMock::API_KEY, claude.base_url());
    let primary = format!("openai:chat-completion?model=gpt-test&apiKey=wrong&baseUrl={}&maxRetries=0&fallbackModels={}", ai.base_url(), fallback);
    let cached = format!("openai:chat-completion?model=gpt-test&apiKey={}&baseUrl={}&responseCache=exact", OpenAiMock::API_KEY, ai.base_url());
    ctx.add_routes(move |b| {
        b.from("direct:primary").to(primary.as_str());
        b.from("direct:cached").to(cached.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:primary", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Echo: hello", "answered by the fallback");
    assert_eq!(ai.server.requests_to("/v1/chat/completions").len(), 1);
    assert_eq!(claude.server.requests_to("/v1/messages").len(), 1);
    let before = ai.server.requests_to("/v1/chat/completions").len();
    let a = t.send("direct:cached", Exchange::with_body(Some(ctx.clone()), "same question"));
    let b = t.send("direct:cached", Exchange::with_body(Some(ctx.clone()), "same question"));
    assert!(a.exception.is_none() && b.exception.is_none(), "{:?} {:?}", a.exception, b.exception);
    assert_eq!(a.body().to_string(), b.body().to_string());
    assert_eq!(ai.server.requests_to("/v1/chat/completions").len(), before + 1, "the second answer came from the cache");
    assert_eq!(b.header("CamelOpenAITotalTokens").and_then(|v| v.as_i64()), Some(0), "a cached answer costs no tokens");
    ctx.stop().unwrap();
}
