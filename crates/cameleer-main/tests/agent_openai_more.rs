//! The `openai`/`llm` component beyond Chat Completions: streaming with the tool loop, the
//! Responses API and Ollama's native `/api/chat`, through the `tags` loop and the agent step,
//! against `OpenAiMock` and `OllamaMock`.
#![cfg(feature = "full")]

use std::sync::{Arc, Mutex};

use cameleer_core::processor::agent::AgentDefinition;
use cameleer_core::prelude::*;
use cameleer_saas::testing::{OllamaMock, OpenAiMock};

fn context() -> CamelContext {
    let main = cameleer_main::new_main();
    main.context().clone()
}

fn tools(b: &mut cameleer_core::dsl::RouteBuilder) {
    b.from("ai-tool:lookup_order?description=Look up an order&tags=orders&parameter.orderId=string&parameter.orderId.required=true&readOnlyHint=true")
        .set_body(simple("order ${header.orderId} is shipped"));
}

const PROMPT: &str = r#"call lookup_order {"orderId":"A1"}; call lookup_order {"orderId":"B2"}"#;
const ANSWER: &str = "Done: order A1 is shipped | order B2 is shipped";

/// A processor bean that collects the streamed deltas.
fn collector(ctx: &CamelContext, name: &str) -> Arc<Mutex<Vec<String>>> {
    let chunks: Arc<Mutex<Vec<String>>> = Default::default();
    let c2 = chunks.clone();
    let p: Arc<dyn cameleer_core::spi::Processor> = Arc::new(move |ex: &mut Exchange| -> Result<()> {
        c2.lock().unwrap().push(ex.body().to_string());
        Ok(())
    });
    ctx.registry().bind(name, p);
    chunks
}

fn kinds(items: &Value) -> Vec<String> {
    items.as_array().unwrap().iter().map(|i| i.get("type").map(|t| t.to_string()).unwrap_or_default()).collect()
}

#[test]
fn responses_api_runs_the_tag_loop() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let base = format!("openai:responses?model=gpt-test&apiKey={}&baseUrl={}", OpenAiMock::API_KEY, ai.base_url());
    let (looped, one, plain) = (format!("{}&tags=orders&systemMessage=terse", base), format!("{}&tags=orders&autoToolExecution=false", base.replace("openai:", "llm:")), format!("{}&systemMessage=terse&maxTokens=50", base));
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:loop").to(looped.as_str());
        b.from("direct:one").to(one.as_str());
        b.from("direct:plain").to(plain.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:loop", Exchange::with_body(Some(ctx.clone()), PROMPT));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(out.header("CamelOpenAIToolIterations").unwrap().as_i64(), Some(2));
    assert_eq!(out.header("CamelOpenAITotalTokens").unwrap().as_i64(), Some(32));
    // the wire: instructions, Responses-shaped tools, then the calls and their outputs
    let reqs = ai.server.requests_to("/v1/responses");
    assert_eq!(reqs.len(), 2);
    let first = reqs[0].json();
    assert_eq!(first.get("instructions").unwrap().to_string(), "terse");
    assert_eq!(first.get("tools").unwrap().as_array().unwrap()[0].get("name").unwrap().to_string(), "lookup_order");
    let second = reqs[1].json();
    let input = second.get("input").unwrap();
    assert_eq!(kinds(input), vec!["message", "function_call", "function_call", "function_call_output", "function_call_output"]);
    assert_eq!(input.as_array().unwrap()[3].get("call_id").unwrap().to_string(), "call_1");
    // autoToolExecution=false: one turn, the requested calls in a header
    let out = t.send("direct:one", Exchange::with_body(Some(ctx.clone()), r#"call lookup_order {"orderId":"C3"}"#));
    assert_eq!(out.header("CamelOpenAIToolCalls").unwrap().to_json(), r#"[{"id":"call_1","name":"lookup_order","arguments":{"orderId":"C3"}}]"#);
    // no tools: plain text, with max_output_tokens
    let out = t.send("direct:plain", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Echo: hello [terse]");
    assert_eq!(out.header("CamelOpenAIFinishReason").unwrap().to_string(), "completed");
    assert_eq!(ai.server.requests_to("/v1/responses").last().unwrap().json().get("max_output_tokens").unwrap().as_i64(), Some(50));
    ctx.stop().unwrap();
}

#[test]
fn responses_api_in_the_agent_step_with_record_and_replay() {
    let ai = OpenAiMock::start();
    let dir = std::env::temp_dir().join(format!("cameleer-agent-responses-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let recording = dir.join("model.jsonl").to_string_lossy().replace('\\', "/");
    let _ = std::fs::remove_file(&recording);
    let run = |model: String| {
        let ctx = context();
        ctx.add_routes(move |b| {
            tools(b);
            b.from("direct:agent").agent(AgentDefinition::new(model.as_str()).tools("orders").max_turns(4));
        })
        .unwrap();
        ctx.start().unwrap();
        let out = ctx.create_producer_template().send("direct:agent", Exchange::with_body(Some(ctx.clone()), PROMPT));
        ctx.stop().unwrap();
        out
    };
    let model = format!("openai:responses?model=gpt-test&apiKey={}&baseUrl={}", OpenAiMock::API_KEY, ai.base_url());
    let out = run(format!("{}&record=file:{}", model, recording));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(out.header("CamelAiAgentTurns").unwrap().as_i64(), Some(2));
    assert_eq!(out.header("CamelAiAgentToolCalls").unwrap().as_i64(), Some(2));
    assert_eq!(std::fs::read_to_string(&recording).unwrap().lines().count(), 2);
    let calls = ai.server.requests_to("/v1/responses").len();
    // replayed: the same answer without the model
    let out = run(format!("openai:responses?model=gpt-test&baseUrl=http://127.0.0.1:9/unused&replay=file:{}", recording));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(ai.server.requests_to("/v1/responses").len(), calls);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn streaming_runs_the_tag_loop_and_passes_the_deltas_on() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let chunks = collector(&ctx, "onChunk");
    let responses_chunks = collector(&ctx, "onResponsesChunk");
    let base = format!("model=gpt-test&apiKey={}&baseUrl={}", OpenAiMock::API_KEY, ai.base_url());
    let looped = format!("openai:chat-completion?{}&tags=orders&streaming=true&streamingChunkProcessor=#onChunk", base);
    let plain = format!("openai:chat-completion?{}&streaming=true&streamingChunkProcessor=#onChunk", base);
    let unstreamed = format!("openai:chat-completion?{}", base);
    let responses = format!("openai:responses?{}&tags=orders&streaming=true&streamingChunkProcessor=#onResponsesChunk", base);
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:loop").to(looped.as_str());
        b.from("direct:plain").to(plain.as_str());
        b.from("direct:unstreamed").to(unstreamed.as_str());
        b.from("direct:responses").to(responses.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:loop", Exchange::with_body(Some(ctx.clone()), PROMPT));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    // the tools still ran between the streamed turns
    assert_eq!(out.header("CamelOpenAIToolIterations").unwrap().as_i64(), Some(2));
    let calls = out.header("CamelOpenAIToolCalls").unwrap();
    assert_eq!(calls.as_array().unwrap().len(), 2);
    assert_eq!(out.header("CamelOpenAITotalTokens").unwrap().as_i64(), Some(32));
    let got = chunks.lock().unwrap().clone();
    assert!(got.len() > 3, "{:?}", got);
    assert_eq!(got.concat(), ANSWER);
    assert_eq!(out.header("CamelOpenAIStreamingChunkCount").unwrap().as_i64(), Some(got.len() as i64));
    let reqs = ai.server.requests_to("/v1/chat/completions");
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        let b = r.json();
        assert!(b.get("stream").unwrap().is_truthy());
        assert!(b.get("stream_options").unwrap().get("include_usage").unwrap().is_truthy());
    }
    // the streamed tool call arguments were put back together
    let second = reqs[1].json();
    let assistant = &second.get("messages").unwrap().as_array().unwrap()[1];
    assert_eq!(assistant.get("tool_calls").unwrap().as_array().unwrap()[1].get("function").unwrap().get("arguments").unwrap().to_string(), r#"{"orderId":"B2"}"#);

    // without tags: a plain streamed chat, with the usual headers
    chunks.lock().unwrap().clear();
    let out = t.send("direct:plain", Exchange::with_body(Some(ctx.clone()), "hello there"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Echo: hello there");
    assert_eq!(chunks.lock().unwrap().clone(), vec!["Echo: ", "hello ", "there"]);
    assert_eq!(out.header("CamelOpenAIResponseId").unwrap().to_string(), "chatcmpl-test");
    assert_eq!(out.header("CamelOpenAIFinishReason").unwrap().to_string(), "stop");
    assert!(out.header("CamelOpenAITotalTokens").unwrap().as_i64().unwrap() > 0);

    // the header streams one call of an endpoint without the option
    let mut ex = Exchange::with_body(Some(ctx.clone()), "hi");
    ex.set_header("CamelOpenAIStreaming", true);
    let out = t.send("direct:unstreamed", ex);
    assert_eq!(out.body().to_string(), "Echo: hi");
    assert_eq!(out.header("CamelOpenAIStreamingChunkCount").unwrap().as_i64(), Some(2));
    assert!(ai.server.requests_to("/v1/chat/completions").last().unwrap().json().get("stream").unwrap().is_truthy());

    // the Responses API streams too
    let out = t.send("direct:responses", Exchange::with_body(Some(ctx.clone()), PROMPT));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(responses_chunks.lock().unwrap().concat(), ANSWER);
    ctx.stop().unwrap();
}

#[test]
fn a_missing_chunk_processor_fails_the_exchange() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let uri = format!("openai:chat-completion?model=gpt-test&apiKey={}&baseUrl={}&streaming=true&streamingChunkProcessor=#nope", OpenAiMock::API_KEY, ai.base_url());
    ctx.add_routes(move |b| {
        b.from("direct:s").to(uri.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let out = ctx.create_producer_template().send("direct:s", Exchange::with_body(Some(ctx.clone()), "hi"));
    let e = out.exception.expect("fails");
    assert!(e.to_string().contains("nope"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn ollama_native_api_runs_the_loop_and_the_agent_step() {
    let ollama = OllamaMock::start();
    let ctx = context();
    let model = format!("openai:chat-completion?model=llama3.2&apiFormat=ollama&baseUrl={}", ollama.base_url());
    let (looped, agent_model, plain) = (format!("{}&tags=orders", model), model.clone(), format!("llm:chat-completion?model=llama3.2&apiFormat=ollama&baseUrl={}/v1&systemMessage=terse&temperature=0&maxTokens=20", ollama.base_url()));
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:loop").to(looped.as_str());
        b.from("direct:agent").agent(AgentDefinition::new(agent_model.as_str()).tools("orders"));
        b.from("direct:plain").to(plain.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:loop", Exchange::with_body(Some(ctx.clone()), PROMPT));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(out.header("CamelOpenAIToolIterations").unwrap().as_i64(), Some(2));
    assert_eq!(out.header("CamelOpenAITotalTokens").unwrap().as_i64(), Some(32));
    let reqs = ollama.server.requests_to("/api/chat");
    let second = reqs[1].json();
    let msgs = second.get("messages").unwrap().as_array().unwrap().clone();
    let roles: Vec<String> = msgs.iter().map(|m| m.get("role").unwrap().to_string()).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "tool"]);
    assert_eq!(msgs[2].get("tool_name").unwrap().to_string(), "lookup_order");
    assert_eq!(msgs[1].get("tool_calls").unwrap().as_array().unwrap()[0].get("function").unwrap().get("arguments").unwrap().to_json(), r#"{"orderId":"A1"}"#);
    // the agent step
    let out = t.send("direct:agent", Exchange::with_body(Some(ctx.clone()), PROMPT));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), ANSWER);
    assert_eq!(out.header("CamelAiAgentToolCalls").unwrap().as_i64(), Some(2));
    // plain chat; a base URL ending in /v1 still reaches /api/chat; the options go in `options`
    let out = t.send("direct:plain", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Echo: hello [terse]");
    assert_eq!(out.header("CamelOpenAIFinishReason").unwrap().to_string(), "stop");
    assert_eq!(out.header("CamelOpenAIPromptTokens").unwrap().as_i64(), Some(12));
    let last = ollama.server.requests_to("/api/chat").last().unwrap().json();
    assert_eq!(last.get("options").unwrap().to_json(), r#"{"num_predict":20,"temperature":0.0}"#);
    assert!(!last.get("stream").unwrap().is_truthy());
    ctx.stop().unwrap();
}

#[test]
fn invalid_api_format_combinations_are_rejected() {
    let ctx = context();
    for uri in [
        "openai:chat-completion?model=m&apiFormat=ollama&streaming=true",
        "openai:embeddings?model=m&apiFormat=ollama",
        "openai:responses?model=m&apiFormat=ollama",
        "openai:chat-completion?model=m&apiFormat=bogus",
    ] {
        assert!(ctx.get_endpoint(uri).is_err(), "{}", uri);
    }
    assert!(ctx.get_endpoint("openai:responses?model=m&streaming=true").is_ok());
}
