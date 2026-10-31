//! cxf / spring-ws / cxfrs end to end: our producers against our consumers, plus raw
//! envelopes as other SOAP stacks send them.

use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_soap::envelope::{parse_envelope, SoapFault};
use camel_soap::testing::*;
use camel_soap::wssec::WSSE_NS;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    camel_soap::register(&ctx);
    ctx.registry().bind("calcWsdl", CALCULATOR_WSDL.to_string());
    ctx.registry().bind("stockWsdl", STOCK_RPC_WSDL.to_string());
    ctx
}

fn svc() -> String {
    format!("serviceName={{{0}}}CalculatorService&portName={{{0}}}CalculatorSoap", CALC_NS)
}

fn wait_for(m: &MockEndpoint, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.received_counter() < n && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(m.received_counter() >= n, "expected {} exchanges, got {}", n, m.received_counter());
}

/// The calculator implementation for POJO consumers.
fn calculator(ex: &mut Exchange) -> Result<()> {
    let op = ex.message.headers.get("operationName").map(|v| v.to_string()).unwrap_or_default();
    let args = ex.message.body.as_array().cloned().unwrap_or_default();
    match op.as_str() {
        "Add" => {
            let s = args.iter().filter_map(|v| v.as_i64()).sum::<i64>();
            ex.message.body = Value::Int(s);
        }
        "Divide" => {
            let (a, b) = (args[0].as_f64().unwrap_or(0.0), args[1].as_f64().unwrap_or(0.0));
            if b == 0.0 {
                return Err(SoapFault::new("Client", "Division by zero")
                    .with_detail(format!("<c:DivideByZeroFault xmlns:c=\"{}\"><c:message>Division by zero</c:message><c:dividend>{}</c:dividend></c:DivideByZeroFault>", CALC_NS, a))
                    .into_error());
            }
            ex.message.body = Value::Float(a / b);
        }
        "Ping" => {}
        other => return Err(CamelError::illegal_argument(format!("unexpected operation {}", other))),
    }
    Ok(())
}

fn start_pojo_calculator(ctx: &CamelContext, extra: &str) -> (String, u16) {
    let uri = format!("cxf://http://127.0.0.1:0/calculator?wsdlURL=#calcWsdl&{}{}", svc(), extra);
    let u = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&u).wire_tap("mock:calls").process(calculator);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_soap::local_port(ctx, &uri).expect("bound port");
    (uri, port)
}

#[test]
fn pojo_soap11_and_soap12_with_faults_and_one_way() {
    let ctx = ctx();
    let (_, port) = start_pojo_calculator(&ctx, "");
    let t = ctx.create_producer_template();
    let p11 = format!("cxf://http://127.0.0.1:{}/calculator?wsdlURL=#calcWsdl&{}", port, svc());
    let p12 = format!("cxf://http://127.0.0.1:{}/calculator?wsdlURL=#calcWsdl&serviceName={{{1}}}CalculatorService&portName=CalculatorSoap12&defaultOperationName=Add", port, CALC_NS);

    // SOAP 1.1, positional parameters
    let r = t.request_body_and_header(&p11, Value::Array(vec![Value::Int(2), Value::Int(3)]), "operationName", "Add").unwrap();
    assert_eq!(r, Value::Int(5));
    // SOAP 1.2, named parameters, default operation
    let r = t.request_body(&p12, Value::object([("a", 40), ("b", 2)])).unwrap();
    assert_eq!(r, Value::Int(42));
    let calls = ctx.mock_endpoint("mock:calls").unwrap();
    wait_for(&calls, 2);
    let rx = calls.received_exchanges();
    assert_eq!(rx[0].message.headers.get("CamelCxfSoapVersion"), Some(&Value::from("1.1")));
    assert_eq!(rx[1].message.headers.get("CamelCxfSoapVersion"), Some(&Value::from("1.2")));
    assert_eq!(rx[0].message.headers.get("SOAPAction"), Some(&Value::from("http://example.org/calculator/Add")));
    assert_eq!(rx[0].message.headers.get("operationNamespace"), Some(&Value::from(CALC_NS)));
    assert_eq!(rx[0].message.body, Value::Array(vec![Value::Int(2), Value::Int(3)]));

    // typed doubles
    let r = t.request_body_and_header(&p11, Value::Array(vec![Value::Float(7.0), Value::Int(2)]), "operationName", "Divide").unwrap();
    assert_eq!(r, Value::Float(3.5));

    // declared fault, both versions
    for p in [&p11, &p12] {
        let ex = t.send_with(p, ExchangePattern::InOut, |ex| {
            ex.set_body(Value::Array(vec![Value::Int(1), Value::Int(0)]));
            ex.set_header("operationName", "Divide");
        });
        let e = ex.exception.clone().expect("fault");
        assert!(e.is_a("SoapFault"), "{:?}", e);
        assert_eq!(e.message, "Division by zero");
        let code = ex.message.headers.get("CamelCxfFaultCode").unwrap().to_string();
        assert!(code == "Client" || code == "Sender", "{}", code);
        assert!(ex.message.headers.get("CamelCxfFaultDetail").unwrap().to_string().contains("DivideByZeroFault"));
    }

    // one-way
    let r = t.request_body_and_header(&p11, Value::from("hello"), "operationName", "Ping").unwrap();
    assert_eq!(r, Value::Null);
    wait_for(&calls, 6);
    let ping = calls.received_exchanges().into_iter().find(|e| e.message.headers.get("operationName") == Some(&Value::from("Ping"))).unwrap();
    assert_eq!(ping.message.body, Value::Array(vec![Value::from("hello")]));

    // unknown operation on the client side
    assert!(t.request_body_and_header(&p11, Value::Null, "operationName", "Nope").is_err());
    ctx.stop().unwrap();
}

#[test]
fn raw_ws_i_envelope_wsdl_and_validation() {
    let ctx = ctx();
    let (_, port) = start_pojo_calculator(&ctx, "&schemaValidationEnabled=true&publishedEndpointUrl=http://calc.example.org/calculator");
    let t = ctx.create_producer_template();
    // a raw envelope as a .NET client sends it (default namespace, whitespace)
    let raw = format!("cxf://http://127.0.0.1:{}/calculator?dataFormat=RAW", port);
    let resp = t.request_body_and_header(&raw, ADD_REQUEST_11, "SOAPAction", "http://example.org/calculator/Add").unwrap().to_string();
    let env = parse_envelope(&resp).unwrap();
    assert!(env.fault.is_none(), "{}", resp);
    assert!(env.body[0].contains(">5</"), "{}", resp);
    // CXF_MESSAGE behaves like RAW, dispatch by body element without SOAPAction
    let msg = format!("cxf://http://127.0.0.1:{}/calculator?dataFormat=CXF_MESSAGE", port);
    let resp = t.request_body(&msg, ADD_REQUEST_11.replace("<a>2</a>", "<a>10</a>")).unwrap().to_string();
    assert!(resp.contains(">13</"), "{}", resp);
    // schema validation
    let bad = t.request_body(&msg, ADD_REQUEST_11.replace("<a>2</a>", "<a>two</a>")).unwrap().to_string();
    let f = parse_envelope(&bad).unwrap().fault.expect("validation fault");
    assert_eq!(f.code, "Client");
    assert!(f.reason.contains("Unmarshalling Error") && f.reason.contains("'two'"), "{}", f.reason);
    // unknown body element
    let unknown = t.request_body(&msg, ADD_REQUEST_11.replace("Add ", "Multiply ").replace("</Add>", "</Multiply>")).unwrap().to_string();
    assert!(parse_envelope(&unknown).unwrap().fault.unwrap().reason.contains("Multiply"));
    // not a SOAP envelope
    let nope = t.request_body(&msg, "<hello/>").unwrap().to_string();
    assert!(parse_envelope(&nope).unwrap().fault.is_some());

    // ?wsdl
    let w = camel_http::HttpClient::new().get(&format!("http://127.0.0.1:{}/calculator?wsdl", port)).unwrap();
    assert_eq!(w.status, 200);
    let text = w.body_text();
    assert!(text.contains("location=\"http://calc.example.org/calculator\""), "{}", text);
    let reparsed = camel_soap::Wsdl::parse(&text, None).unwrap();
    assert_eq!(reparsed.operations(None, None).unwrap().len(), 3);
    // GET without ?wsdl
    assert_eq!(camel_http::HttpClient::new().get(&format!("http://127.0.0.1:{}/calculator", port)).unwrap().status, 405);
    ctx.stop().unwrap();
}

#[test]
fn payload_and_raw_consumers() {
    let ctx = ctx();
    let payload = format!("cxf://http://127.0.0.1:0/payload?wsdlURL=#calcWsdl&dataFormat=PAYLOAD&{}", svc());
    let raw = "cxf://http://127.0.0.1:0/raw?dataFormat=RAW".to_string();
    let (p2, r2) = (payload.clone(), raw.clone());
    ctx.add_routes(move |b| {
        b.from(&p2).process(|ex: &mut Exchange| {
            let req = ex.message.body.to_string();
            assert!(req.contains("Add"), "{}", req);
            let doc = camel_xml::Document::parse(&req).unwrap();
            let sum: i64 = doc.element_children(doc.document_element().unwrap()).iter().map(|&c| doc.string_value(c).trim().parse::<i64>().unwrap()).sum();
            ex.message.body = Value::String(format!("<r:AddResponse xmlns:r=\"{}\"><r:AddResult>{}</r:AddResult></r:AddResponse>", CALC_NS, sum));
            Ok(())
        });
        b.from(&r2).process(|ex: &mut Exchange| {
            let env = ex.message.body.to_string();
            if env.contains("fail") {
                ex.message.body = Value::from(SOAP12_FAULT);
            } else {
                ex.message.body = Value::from(env.replace("ping", "pong"));
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let (pp, rp) = (camel_soap::local_port(&ctx, &payload).unwrap(), camel_soap::local_port(&ctx, &raw).unwrap());
    let t = ctx.create_producer_template();
    // PAYLOAD producer -> PAYLOAD consumer
    let prod = format!("cxf://http://127.0.0.1:{}/payload?wsdlURL=#calcWsdl&dataFormat=PAYLOAD&defaultOperationName=Add", pp);
    let r = t.request_body(&prod, format!("<c:Add xmlns:c=\"{}\"><c:a>4</c:a><c:b>5</c:b></c:Add>", CALC_NS)).unwrap();
    assert_eq!(r.to_string(), format!("<r:AddResponse xmlns:r=\"{}\"><r:AddResult>9</r:AddResult></r:AddResponse>", CALC_NS));
    // POJO producer -> PAYLOAD consumer
    let pojo = format!("cxf://http://127.0.0.1:{}/payload?wsdlURL=#calcWsdl&defaultOperationName=Add", pp);
    assert_eq!(t.request_body(&pojo, Value::Array(vec![Value::Int(1), Value::Int(2)])).unwrap(), Value::Int(3));
    // RAW producer -> RAW consumer (no WSDL)
    let rprod = format!("cxf://http://127.0.0.1:{}/raw?dataFormat=RAW", rp);
    let env = camel_soap::envelope::build_envelope(camel_soap::SoapVersion::V11, &[], &["<m:ping xmlns:m=\"urn:m\"/>".into()]);
    let r = t.request_body(&rprod, env).unwrap().to_string();
    assert!(r.contains("<m:pong"), "{}", r);
    // a hand-written SOAP 1.2 fault returned by a RAW route reaches a PAYLOAD producer as a SoapFault
    let pprod = format!("cxf://http://127.0.0.1:{}/raw?dataFormat=PAYLOAD&soapVersion=1.2", rp);
    let ex = t.send_with(&pprod, ExchangePattern::InOut, |ex| ex.set_body("<m:fail xmlns:m=\"urn:m\"/>"));
    let e = ex.exception.clone().expect("fault");
    assert_eq!(e.message, "Division by zero");
    assert_eq!(ex.message.headers.get("CamelCxfFaultCode"), Some(&Value::from("Sender")));
    let f = SoapFault::from_error(&e, camel_soap::SoapVersion::V12);
    assert_eq!(f.subcode.as_deref(), Some(format!("{{{}}}DivideByZero", CALC_NS).as_str()));
    assert!(f.detail.unwrap().contains("<m:dividend>1</m:dividend>"));
    ctx.stop().unwrap();
}

#[test]
fn rpc_literal_and_no_wsdl_pojo() {
    let ctx = ctx();
    let stock = "cxf://http://127.0.0.1:0/stock?wsdlURL=#stockWsdl".to_string();
    let echo = "cxf://http://127.0.0.1:0/echo".to_string();
    let (s2, e2) = (stock.clone(), echo.clone());
    ctx.add_routes(move |b| {
        b.from(&s2).process(|ex: &mut Exchange| {
            assert_eq!(ex.message.headers.get("operationName"), Some(&Value::from("GetLastTradePrice")));
            assert_eq!(ex.message.headers.get("operationNamespace"), Some(&Value::from("urn:example:stock")));
            let sym = ex.message.body.as_array().unwrap()[0].to_string();
            ex.message.body = Value::Float(if sym == "ACME" { 34.5 } else { 0.0 });
            Ok(())
        });
        b.from(&e2).process(|ex: &mut Exchange| {
            let args = ex.message.body.as_array().cloned().unwrap_or_default();
            ex.message.body = Value::String(format!("{}:{}", ex.message.headers.get("operationName").unwrap(), args.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(",")));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let sp = camel_soap::local_port(&ctx, &stock).unwrap();
    let r = t.request_body(&format!("cxf://http://127.0.0.1:{}/stock?wsdlURL=#stockWsdl", sp), "ACME").unwrap();
    assert_eq!(r, Value::Float(34.5));
    let ep = camel_soap::local_port(&ctx, &echo).unwrap();
    let r = t.request_body(&format!("cxf://http://127.0.0.1:{}/echo?defaultOperationName=echo&defaultOperationNamespace=urn:echo", ep), Value::Array(vec![Value::from("a"), Value::Int(1)])).unwrap();
    assert_eq!(r, Value::from("echo:a,1"));
    ctx.stop().unwrap();
}

#[test]
fn ws_security_username_token_and_signature() {
    let ctx = ctx();
    let id = &camel_tls::testing::pki().localhost;
    ctx.registry().bind("sigKey", id.key_pem.clone());
    ctx.registry().bind("sigCert", id.cert_pem.clone());
    let (_, port) = start_pojo_calculator(&ctx, "&wsSecurityUsers=alice:secret,bob:pw");
    let t = ctx.create_producer_template();
    let base = format!("cxf://http://127.0.0.1:{}/calculator?wsdlURL=#calcWsdl&defaultOperationName=Add", port);
    let args = || Value::Array(vec![Value::Int(1), Value::Int(1)]);
    // accepted: text and digest
    assert_eq!(t.request_body(&format!("{}&wsSecurityUsername=alice&wsSecurityPassword=secret", base), args()).unwrap(), Value::Int(2));
    assert_eq!(t.request_body(&format!("{}&properties.ws-security.username=bob&properties.ws-security.password=pw&wsSecurityPasswordType=PasswordDigest", base), args()).unwrap(), Value::Int(2));
    // rejected
    for uri in [format!("{}&wsSecurityUsername=alice&wsSecurityPassword=wrong&wsSecurityPasswordType=PasswordDigest", base), format!("{}&wsSecurityUsername=mallory&wsSecurityPassword=x", base), base.clone()] {
        let ex = t.send_with(&uri, ExchangePattern::InOut, |ex| ex.set_body(args()));
        let e = ex.exception.clone().expect("rejected");
        assert!(e.is_a("SoapFault"));
        let code = ex.message.headers.get("CamelCxfFaultCode").unwrap().to_string();
        assert!(code.starts_with(&format!("{{{}}}", WSSE_NS)), "{}", code);
    }
    ctx.stop().unwrap();

    // signature required
    let ctx = self::ctx();
    ctx.registry().bind("sigKey", id.key_pem.clone());
    ctx.registry().bind("sigCert", id.cert_pem.clone());
    let (_, port) = start_pojo_calculator(&ctx, "&wsSecuritySignatureVerifyCertificate=#sigCert");
    let t = ctx.create_producer_template();
    let base = format!("cxf://http://127.0.0.1:{}/calculator?wsdlURL=#calcWsdl&defaultOperationName=Add", port);
    assert_eq!(t.request_body(&format!("{}&wsSecuritySignatureKey=#sigKey&wsSecuritySignatureCertificate=#sigCert", base), args()).unwrap(), Value::Int(2));
    let e = t.request_body(&base, args()).unwrap_err();
    assert!(format!("{:?}", e).contains("InvalidSecurity"), "{:?}", e);
    ctx.stop().unwrap();
}

#[test]
fn spring_ws_routing_and_producer() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("spring-ws:rootqname:{urn:orders}GetOrder?address=http://127.0.0.1:0/ws").process(|ex: &mut Exchange| {
            let req = ex.message.body.to_string();
            let id = req.split("<o:id>").nth(1).and_then(|s| s.split('<').next()).unwrap_or("?").to_string();
            ex.message.body = Value::String(format!("<o:Order xmlns:o=\"urn:orders\"><o:id>{}</o:id><o:status>shipped</o:status></o:Order>", id));
            Ok(())
        });
        b.from("spring-ws:soapaction:urn:cancel?address=http://127.0.0.1:0/ws").process(|ex: &mut Exchange| {
            assert_eq!(ex.message.headers.get("CamelSpringWebserviceSoapAction"), Some(&Value::from("urn:cancel")));
            Err(CamelError::runtime("cannot cancel a shipped order"))
        });
        b.from("spring-ws:xpathresult:urgent?expression=//*[local-name()='priority']&address=http://127.0.0.1:0/ws").set_body(constant("<ack xmlns=\"urn:orders\">urgent</ack>"));
        b.from("spring-ws:action:urn:orders/Notify?address=http://127.0.0.1:0/ws").to("mock:notify").set_body(constant(Value::Null));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_soap::local_port(&ctx, "spring-ws:rootqname:{urn:orders}GetOrder?address=http://127.0.0.1:0/ws").unwrap();
    assert_eq!(camel_soap::local_port(&ctx, "spring-ws:soapaction:urn:cancel?address=http://127.0.0.1:0/ws"), Some(port), "shared dispatcher");
    let t = ctx.create_producer_template();
    let url = format!("spring-ws:http://127.0.0.1:{}/ws", port);
    let r = t.request_body(&url, "<o:GetOrder xmlns:o=\"urn:orders\"><o:id>42</o:id></o:GetOrder>").unwrap();
    assert_eq!(r.to_string(), "<o:Order xmlns:o=\"urn:orders\"><o:id>42</o:id><o:status>shipped</o:status></o:Order>");
    // by SOAP action; route exception -> fault -> SoapFaultClientException
    let e = t.request_body(&format!("{}?soapAction=urn:cancel", url), "<o:Cancel xmlns:o=\"urn:orders\"/>").unwrap_err();
    let fault = e.chain().find(|x| x.kind == "SoapFaultClientException").expect("SoapFaultClientException");
    assert_eq!(fault.message, "cannot cancel a shipped order");
    // by XPath result, SOAP 1.2
    let r = t.request_body(&format!("{}?soapVersion=1.2", url), "<o:Ship xmlns:o=\"urn:orders\"><o:priority>urgent</o:priority></o:Ship>").unwrap();
    assert!(r.to_string().contains(">urgent</ack>"));
    // WS-Addressing action, one-way style reply
    let r = t.request_body(&format!("{}?wsAddressingAction=urn:orders/Notify", url), "<o:Notify xmlns:o=\"urn:orders\"/>").unwrap();
    assert_eq!(r, Value::Null);
    let notify = ctx.mock_endpoint("mock:notify").unwrap();
    wait_for(&notify, 1);
    assert_eq!(notify.received_exchanges()[0].message.headers.get("CamelSpringWebserviceAddressingAction"), Some(&Value::from("urn:orders/Notify")));
    // no endpoint mapping -> 404
    let e = t.request_body(&url, "<o:Unknown xmlns:o=\"urn:orders\"/>").unwrap_err();
    assert!(e.chain().any(|x| x.is_a("HttpOperationFailedException")), "{:?}", e);
    ctx.stop().unwrap();
}

#[test]
fn cxfrs_consumer_and_producer() {
    let ctx = ctx();
    let uri = "cxfrs:http://127.0.0.1:0/api?resourceClasses=org.example.CustomerService&operations=getCustomer:GET:/customers/{id};addCustomer:POST:/customers";
    ctx.add_routes(move |b| {
        b.from(uri).process(|ex: &mut Exchange| {
            let op = ex.message.headers.get("operationName").unwrap().to_string();
            match op.as_str() {
                "getCustomer" => {
                    let id = ex.message.headers.get("id").unwrap().to_string();
                    if id == "404" {
                        ex.message.headers.set("CamelHttpResponseCode", 404);
                        return Err(CamelError::runtime("no such customer"));
                    }
                    ex.message.body = Value::object([("id", Value::from(id)), ("name", Value::from("Ada")), ("verbose", ex.message.headers.get("verbose").cloned().unwrap_or(Value::Null))]);
                }
                "addCustomer" => {
                    assert_eq!(ex.message.headers.get("CamelHttpPath"), Some(&Value::from("/customers")));
                    let name = ex.message.body.get("name").cloned().unwrap_or(Value::Null);
                    ex.message.headers.set("CamelHttpResponseCode", 201);
                    ex.message.body = Value::object([("id", Value::from("7")), ("name", name)]);
                }
                _ => unreachable!(),
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_soap::local_port(&ctx, uri).unwrap();
    let t = ctx.create_producer_template();
    let prod = format!("cxfrs:http://127.0.0.1:{}/api?parseJson=true", port);
    let ex = t.send_with(&prod, ExchangePattern::InOut, |ex| {
        ex.set_header("CamelHttpMethod", "GET");
        ex.set_header("CamelHttpPath", "/customers/12");
        ex.set_header("CamelHttpQuery", "verbose=yes");
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.message.body.get("name"), Some(&Value::from("Ada")));
    assert_eq!(ex.message.body.get("verbose"), Some(&Value::from("yes")));
    let ex = t.send_with(&prod, ExchangePattern::InOut, |ex| {
        ex.set_header("CamelHttpPath", "/customers");
        ex.set_body(Value::object([("name", "Grace")]));
    });
    assert_eq!(ex.message.headers.get("CamelHttpResponseCode"), Some(&Value::Int(201)));
    assert_eq!(ex.message.body.get("name"), Some(&Value::from("Grace")));
    let e = t.request_body_and_header(&prod, Value::Null, "CamelHttpPath", "/customers/404").unwrap_err();
    assert!(e.message.contains("404"), "{}", e.message);
    let e = t.request_body_and_header(&prod, Value::Null, "CamelHttpPath", "/orders").unwrap_err();
    assert!(e.message.contains("404"), "{}", e.message);
    ctx.stop().unwrap();
}

#[test]
fn hand_written_soap12_fault_parses() {
    let env = parse_envelope(SOAP12_FAULT).unwrap();
    let f = env.fault.unwrap();
    assert_eq!(f.code, "Sender");
    assert_eq!(f.subcode.as_deref(), Some(format!("{{{}}}DivideByZero", CALC_NS).as_str()));
    assert_eq!(f.reason, "Division by zero");
    assert!(f.detail.unwrap().starts_with("<m:DivideByZeroFault"));
}
