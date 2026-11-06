//! Tool calling in the model components and the `agent` step, against the provider mocks.
#![cfg(feature = "full")]

use std::time::Duration;

use cameleer_core::processor::agent::AgentDefinition;
use cameleer_core::prelude::*;
use cameleer_saas::testing::OpenAiMock;

fn context() -> CamelContext {
    let main = cameleer_main::new_main();
    main.context().clone()
}

fn tools(b: &mut cameleer_core::dsl::RouteBuilder) {
    b.from("ai-tool:lookup_order?description=Look up an order&tags=orders&parameter.orderId=string&parameter.orderId.required=true&readOnlyHint=true")
        .set_body(simple("order ${header.orderId} is shipped"));
    b.from("ai-tool:refund?description=Refund an order&tags=orders&parameter.orderId=string&parameter.orderId.required=true&destructiveHint=true")
        .to("mock:refunds")
        .set_body(simple("refunded ${header.orderId}"));
}

#[test]
fn openai_runs_the_tool_loop_with_tags() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let uri = format!("openai:chat-completion?model=gpt-test&apiKey={}&baseUrl={}&tags=orders", OpenAiMock::API_KEY, ai.base_url());
    let llm = format!("llm:chat-completion?model=gpt-test&apiKey={}&baseUrl={}&tags=orders&autoToolExecution=false", OpenAiMock::API_KEY, ai.base_url());
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:ask").to(uri.as_str());
        b.from("direct:one-turn").to(llm.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let out = t.send("direct:ask", Exchange::with_body(Some(ctx.clone()), r#"call lookup_order {"orderId":"A1"}; call lookup_order {"orderId":"B2"}"#));
    assert!(out.exception.is_none(), "{:?}", out.exception);
    assert_eq!(out.body().to_string(), "Done: order A1 is shipped | order B2 is shipped");
    assert_eq!(out.header("CamelOpenAIToolIterations").unwrap().as_i64(), Some(2));
    assert_eq!(out.header("CamelOpenAIToolStopReason").unwrap().to_string(), "answer");
    assert_eq!(out.header("CamelOpenAITotalTokens").unwrap().as_i64(), Some(32));
    // a hallucinated tool is reported to the model, which answers with that
    let out = t.send("direct:ask", Exchange::with_body(Some(ctx.clone()), r#"call delete_everything {}"#));
    assert!(out.body().to_string().starts_with("Done: There is no tool named delete_everything"), "{}", out.body());
    // autoToolExecution=false (through the llm alias): the calls come back in a header
    let out = t.send("direct:one-turn", Exchange::with_body(Some(ctx.clone()), r#"call lookup_order {"orderId":"C3"}"#));
    let calls = out.header("CamelOpenAIToolCalls").unwrap().to_json();
    assert_eq!(calls, r#"[{"id":"call_1","name":"lookup_order","arguments":{"orderId":"C3"}}]"#);
    ctx.stop().unwrap();
}

#[test]
fn the_agent_step_pauses_for_approval_and_resumes() {
    let ai = OpenAiMock::start();
    let ctx = context();
    let model = format!("openai:chat-completion?model=gpt-test&apiKey={}&baseUrl={}", OpenAiMock::API_KEY, ai.base_url());
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:agent").agent(AgentDefinition::new(model.as_str()).tools("orders").memory("memory").on_approval("mock:approvals").max_turns(5));
        b.from("direct:approve").set_header("CamelAiToolApprovedBy", constant("alice")).to("ai-tool:refund?operation=approve");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ask = |body: &str| {
        let mut ex = Exchange::with_body(Some(ctx.clone()), body);
        ex.set_header("CamelAiConversationId", "conv-1");
        t.send("direct:agent", ex)
    };
    let first = ask(r#"call refund {"orderId":"A1"}"#);
    assert!(first.exception.is_none(), "{:?}", first.exception);
    assert_eq!(first.header("CamelAiAgentStopReason").unwrap().to_string(), "approval-required");
    let handle = first.header("CamelAiApprovalHandle").unwrap().to_string();
    assert_eq!(ctx.mock_endpoint("mock:refunds").unwrap().received_counter(), 0);
    let notes = ctx.mock_endpoint("mock:approvals").unwrap();
    assert_eq!(notes.received_counter(), 1);
    assert_eq!(notes.received_exchanges()[0].body().to_string(), r#"{"orderId":"A1"}"#);
    // still waiting
    let waiting = ask("anything");
    assert_eq!(waiting.header("CamelAiAgentStopReason").unwrap().to_string(), "approval-pending");
    // a person approves; the refund runs once
    let mut approve = Exchange::new(Some(ctx.clone()));
    approve.set_header("CamelAiToolApprovalHandle", handle.as_str());
    let approved = t.send("direct:approve", approve);
    assert_eq!(approved.body().to_string(), "refunded A1");
    assert_eq!(ctx.mock_endpoint("mock:refunds").unwrap().received_counter(), 1);
    // the conversation resumes with the result; the model answers
    let resumed = ask("continue");
    assert_eq!(resumed.header("CamelAiAgentStopReason").unwrap().to_string(), "answer", "{}", resumed.body());
    assert_eq!(resumed.body().to_string(), "Done: refunded A1");
    assert_eq!(ctx.mock_endpoint("mock:refunds").unwrap().received_counter(), 1);
    ctx.stop().unwrap();
}

#[test]
fn the_agent_step_in_yaml_with_record_and_replay() {
    let ai = OpenAiMock::start();
    let base_url = ai.base_url();
    let dir = std::env::temp_dir().join(format!("cameleer-agent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let recording = dir.join("model.jsonl").to_string_lossy().replace('\\', "/");
    let _ = std::fs::remove_file(&recording);
    let yaml = |extra: &str| {
        format!(
            r#"
- route:
    from:
      uri: ai-tool:lookup_order
      parameters:
        tags: orders
        parameter.orderId: string
        readOnlyHint: true
      steps:
        - setBody:
            simple: "order ${{header.orderId}} is shipped"
- route:
    from:
      uri: direct:agent
      steps:
        - agent:
            model: "openai:chat-completion?model=gpt-test&apiKey={key}&baseUrl={base}"
            tools: orders
            systemMessage: You are a support agent.
            maxTurns: 4
            {extra}
"#,
            key = OpenAiMock::API_KEY,
            base = base_url,
            extra = extra
        )
    };
    let run = |yaml: String| {
        let ctx = context();
        cameleer_yaml::add_routes_from_yaml(&ctx, &yaml).unwrap();
        ctx.start().unwrap();
        let out = ctx.create_producer_template().send("direct:agent", Exchange::with_body(Some(ctx.clone()), r#"call lookup_order {"orderId":"Z9"}"#));
        ctx.stop().unwrap();
        out
    };
    let recorded = run(yaml(&format!("record: \"file:{}\"", recording)));
    assert!(recorded.exception.is_none(), "{:?}", recorded.exception);
    assert_eq!(recorded.body().to_string(), "Done: order Z9 is shipped");
    assert_eq!(recorded.header("CamelAiAgentTurns").unwrap().as_i64(), Some(2));
    assert_eq!(std::fs::read_to_string(&recording).unwrap().lines().count(), 2);
    // replay: the same route runs without the model
    drop(ai);
    let replayed = run(yaml(&format!("replay: \"file:{}\"", recording)).replace("baseUrl=http", "baseUrl=http://127.0.0.1:9/unused?x="));
    assert!(replayed.exception.is_none(), "{:?}", replayed.exception);
    assert_eq!(replayed.body().to_string(), "Done: order Z9 is shipped");
    assert_eq!(replayed.header("CamelAiAgentToolCalls").unwrap().as_i64(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = Duration::ZERO;
}

/// One provider through the `tags` tool loop and the agent step: two parallel calls, the
/// results back with their ids, and the answer.
fn check_provider(name: &str, model: String) {
    let ctx = context();
    let m2 = model.clone();
    let tagged = if model.contains('?') { format!("{}&tags=orders", model) } else { format!("{}?tags=orders", model) };
    ctx.add_routes(move |b| {
        tools(b);
        b.from("direct:loop").to(tagged.as_str());
        b.from("direct:agent").agent(AgentDefinition::new(m2.as_str()).tools("orders"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let prompt = r#"call lookup_order {"orderId":"A1"}; call lookup_order {"orderId":"B2"}"#;
    let out = t.send("direct:loop", Exchange::with_body(Some(ctx.clone()), prompt));
    assert!(out.exception.is_none(), "{}: {:?}", name, out.exception);
    assert_eq!(out.body().to_string(), "Done: order A1 is shipped | order B2 is shipped", "{}", name);
    assert_eq!(out.header("CamelAiAgentTurns").unwrap().as_i64(), Some(2), "{}", name);
    assert_eq!(out.header("CamelAiAgentToolCalls").unwrap().as_i64(), Some(2), "{}", name);
    let out = t.send("direct:agent", Exchange::with_body(Some(ctx.clone()), prompt));
    assert!(out.exception.is_none(), "{}: {:?}", name, out.exception);
    assert_eq!(out.body().to_string(), "Done: order A1 is shipped | order B2 is shipped", "{}", name);
    assert!(out.header("CamelAiAgentTotalTokens").unwrap().as_i64().unwrap() > 0, "{}", name);
    ctx.stop().unwrap();
}

#[test]
fn anthropic_calls_tools() {
    let m = cameleer_saas::testing::AnthropicMock::start();
    let uri = format!("anthropic:messages?model=claude-test&apiKey={}&baseUrl={}", cameleer_saas::testing::AnthropicMock::API_KEY, m.base_url());
    check_provider("anthropic", uri.clone());
    // without tools: plain text with the Anthropic headers
    let ctx = context();
    let u = format!("{}&systemMessage=terse", uri);
    ctx.add_routes(move |b| {
        b.from("direct:chat").to(u.as_str());
    })
    .unwrap();
    ctx.start().unwrap();
    let out = ctx.create_producer_template().send("direct:chat", Exchange::with_body(Some(ctx.clone()), "hello"));
    assert_eq!(out.body().to_string(), "Echo: hello [terse]");
    assert_eq!(out.header("CamelAnthropicInputTokens").unwrap().as_i64(), Some(11));
    ctx.stop().unwrap();
}

#[test]
fn azure_openai_calls_tools() {
    let az = cameleer_cloud::testing::AzureMock::start().unwrap();
    az.add_openai_deployment("gpt-4o");
    check_provider("azure-openai", format!("azure-openai://res/gpt-4o?{}", az.openai_options()));
}

#[test]
fn aws_bedrock_calls_tools() {
    let aws = cameleer_cloud::testing::AwsMock::start().unwrap();
    check_provider("aws-bedrock", format!("aws-bedrock://b?modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=converse&{}", aws.endpoint_options()));
}

#[test]
fn google_vertexai_calls_tools_and_keeps_thought_signatures() {
    let gcp = cameleer_cloud::testing::GcpMock::start().unwrap();
    let key = gcp.write_service_account_file().unwrap();
    check_provider("google-vertexai", format!("google-vertexai:{}:us-central1:gemini-2.5-flash?{}", cameleer_cloud::testing::gcp::PROJECT, gcp.endpoint_options(&key)));
    let _ = std::fs::remove_file(&key);
}
