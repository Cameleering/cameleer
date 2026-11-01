//! FHIR: decimal lexemes, spec element order, versions, profile validation and the fhir
//! component's operation / patch / meta / validate APIs against the embedded mock server.

use std::collections::BTreeMap;

use camel_core::dataformat::create_data_format_with;
use camel_core::prelude::*;
use camel_soap::testing::FhirMock;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    camel_soap::register(&ctx);
    ctx
}

fn df(ctx: &CamelContext, name: &str, opts: &[(&str, &str)]) -> std::sync::Arc<dyn camel_core::dataformat::DataFormat> {
    let o: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    create_data_format_with(ctx.registry(), name, &o).unwrap()
}

fn run(ctx: &CamelContext, d: &std::sync::Arc<dyn camel_core::dataformat::DataFormat>, marshal: bool, body: impl Into<Value>) -> Value {
    let mut ex = Exchange::with_body(Some(ctx.clone()), body);
    if marshal {
        d.marshal(&mut ex).unwrap();
    } else {
        d.unmarshal(&mut ex).unwrap();
    }
    ex.message.body
}

#[test]
fn decimals_keep_their_lexeme_through_values() {
    let ctx = ctx();
    let json = df(&ctx, "fhirJson", &[]);
    let xml = df(&ctx, "fhirXml", &[]);
    let text = r#"{"resourceType":"Observation","status":"final","code":{"text":"w"},"valueQuantity":{"value":185.00,"unit":"lbs"},"component":[{"code":{"text":"x"},"valueQuantity":{"value":1.5}}]}"#;
    let v = run(&ctx, &json, false, text);
    assert_eq!(v.get("valueQuantity").and_then(|q| q.get("value")), Some(&Value::from("185.00")));
    let out = run(&ctx, &json, true, v.clone()).to_string();
    assert!(out.contains("\"value\":185.00"), "{}", out);
    assert!(out.contains("\"value\":1.5"), "{}", out);
    let x = run(&ctx, &xml, true, v).to_string();
    assert!(x.contains("<value value=\"185.00\"/>"), "{}", x);
    // hand-built values: strings in numeric / boolean positions are retyped
    let mut o = camel_core::value::ObjectMap::new();
    o.insert("resourceType".into(), Value::from("Patient"));
    o.insert("active".into(), Value::from("true"));
    o.insert("multipleBirthInteger".into(), Value::from("2"));
    let out = run(&ctx, &json, true, Value::Object(o)).to_string();
    assert_eq!(out, r#"{"resourceType":"Patient","active":true,"multipleBirthInteger":2}"#);
}

#[test]
fn xml_output_follows_spec_order() {
    let ctx = ctx();
    let xml = df(&ctx, "fhirXml", &[]);
    let text = r#"{"resourceType":"Patient","birthDate":"1970-01-01","name":[{"given":["A"],"family":"B","use":"official"}],"extension":[{"url":"u","valueString":"x"}],"id":"p1","gender":"male","meta":{"versionId":"1"},"identifier":[{"value":"1"}]}"#;
    let out = run(&ctx, &xml, true, text).to_string();
    assert_eq!(
        out,
        r#"<Patient xmlns="http://hl7.org/fhir"><id value="p1"/><meta><versionId value="1"/></meta><extension url="u"><valueString value="x"/></extension><identifier><value value="1"/></identifier><name><use value="official"/><family value="B"/><given value="A"/></name><gender value="male"/><birthDate value="1970-01-01"/></Patient>"#
    );
    // version specific registries: DSTU3 Condition.clinicalStatus is a code (JSON string, not repeating)
    let d3 = df(&ctx, "fhirJson", &[("fhirVersion", "DSTU3")]);
    let c = run(&ctx, &d3, true, r#"<Condition xmlns="http://hl7.org/fhir"><clinicalStatus value="active"/><subject><reference value="Patient/1"/></subject></Condition>"#).to_string();
    assert_eq!(c, r#"{"resourceType":"Condition","clinicalStatus":"active","subject":{"reference":"Patient/1"}}"#);
    let r5 = df(&ctx, "fhirJson", &[("fhirVersion", "R5")]);
    let e = run(&ctx, &r5, true, r#"<Encounter xmlns="http://hl7.org/fhir"><status value="planned"/><class><text value="amb"/></class></Encounter>"#).to_string();
    assert_eq!(e, r#"{"resourceType":"Encounter","status":"planned","class":[{"text":"amb"}]}"#);
}

const PROFILE: &str = r#"{"resourceType":"StructureDefinition","url":"http://example.org/fhir/StructureDefinition/strict-patient","type":"Patient","differential":{"element":[{"path":"Patient"},{"path":"Patient.identifier","min":1},{"path":"Patient.birthDate","min":1}]}}"#;

#[test]
fn component_operation_patch_meta_validate() {
    let mock = FhirMock::start();
    let ctx = ctx();
    ctx.registry().bind("strictPatient", Value::from(PROFILE));
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ep = |m: &str, extra: &str| format!("fhir://{}?serverUrl={}{}", m, mock.url(), extra);

    let created = t.request_body(&ep("create/resource", ""), r#"{"resourceType":"Patient","name":[{"family":"Doe"}],"gender":"female"}"#).unwrap();
    let id = format!("Patient/{}", created.get("id").unwrap());
    t.request_body(&ep("create/resource", ""), format!(r#"{{"resourceType":"Observation","status":"final","code":{{"text":"t"}},"subject":{{"reference":"{}"}}}}"#, id)).unwrap();

    // operations: instance $everything, $echo at every level, GET and POST
    let all = t.request_body_and_header(&ep("operation/onInstance", "&name=everything"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(all.get("total"), Some(&Value::Int(2)));
    let mut params = camel_core::value::ObjectMap::new();
    params.insert("greeting".into(), Value::from("hi"));
    params.insert("n".into(), Value::Int(3));
    let echo = t.request_body(&ep("operation/onServer", "&name=$echo"), Value::Object(params.clone())).unwrap();
    let ps = echo.get("parameter").unwrap().as_array().unwrap().clone();
    assert_eq!(ps[0].get("valueString"), Some(&Value::from("hi")));
    assert_eq!(ps[1].get("valueInteger"), Some(&Value::Int(3)));
    assert_eq!(ps[2].get("valueString"), Some(&Value::from("server")));
    let echo = t.request_body(&ep("operation/onType", "&name=echo&resourceType=Patient&useHttpGet=true"), Value::Object(params)).unwrap();
    assert!(mock.requests().iter().any(|r| r.starts_with("GET /fhir/Patient/$echo?greeting=hi&n=3")), "{:?}", mock.requests());
    assert_eq!(echo.get("parameter").unwrap().as_array().unwrap().last().unwrap().get("valueString"), Some(&Value::from("Patient")));
    let echo = t.request_body_and_header(&ep("operation/onInstanceVersion", "&name=echo&version=1"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(echo.get("parameter").unwrap().as_array().unwrap().last().unwrap().get("valueString"), Some(&Value::from(format!("{}/_history/1", id))));
    let msg = t.request_body(&ep("operation/processMessage", ""), r#"{"resourceType":"Bundle","type":"message","entry":[{"resource":{"resourceType":"MessageHeader","id":"h1","eventCoding":{"code":"x"},"source":{"endpoint":"urn:a"}}}]}"#).unwrap();
    assert_eq!(msg.get("type"), Some(&Value::from("message")));

    // patch: JSON Patch, FHIRPath Patch, conditional
    let p = t.request_body_and_header(&ep("patch/withBody", ""), r#"[{"op":"replace","path":"/gender","value":"other"},{"op":"add","path":"/birthDate","value":"1980-02-03"}]"#, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(p.get("gender"), Some(&Value::from("other")));
    assert_eq!(p.get("meta").and_then(|m| m.get("versionId")), Some(&Value::from("2")));
    let fp = r#"{"resourceType":"Parameters","parameter":[{"name":"operation","part":[{"name":"type","valueCode":"add"},{"name":"path","valueString":"Patient"},{"name":"name","valueString":"identifier"},{"name":"value","part":[{"name":"system","valueUri":"urn:x"},{"name":"value","valueString":"42"}]}]},{"name":"operation","part":[{"name":"type","valueCode":"replace"},{"name":"path","valueString":"Patient.name[0].family"},{"name":"value","valueString":"Smith"}]}]}"#;
    let p = t.request_body_and_header(&ep("patch/withBody", ""), fp, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(p.get("identifier").unwrap().as_array().unwrap()[0].get("value"), Some(&Value::from("42")));
    assert_eq!(p.get("name").unwrap().as_array().unwrap()[0].get("family"), Some(&Value::from("Smith")));
    let p = t.request_body(&ep("patch/withUrl", "&url=Patient?family=smith"), r#"[{"op":"remove","path":"/gender"}]"#).unwrap();
    assert!(p.get("gender").is_none());
    let err = t.request_body_and_header(&ep("patch/withBody", ""), r#"[{"op":"test","path":"/gender","value":"x"}]"#, "CamelFhir.id", id.clone()).unwrap_err();
    assert!(format!("{:?}", err).contains("UnprocessableEntityException"), "{:?}", err);

    // meta
    let meta = t.request_body_and_header(&ep("meta/add", ""), r#"{"tag":[{"system":"urn:t","code":"vip"}],"profile":["http://example.org/p"]}"#, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(meta.get("tag").unwrap().as_array().unwrap().len(), 1);
    let meta = t.request_body_and_header(&ep("meta/getFromResource", ""), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(meta.get("profile").unwrap().as_array().unwrap()[0], Value::from("http://example.org/p"));
    let meta = t.request_body(&ep("meta/getFromType", "&resourceType=Patient"), Value::Null).unwrap();
    assert_eq!(meta.get("tag").unwrap().as_array().unwrap()[0].get("code"), Some(&Value::from("vip")));
    let meta = t.request_body(&ep("meta/getFromServer", ""), Value::Null).unwrap();
    assert!(meta.get("tag").is_some());
    let meta = t.request_body_and_header(&ep("meta/delete", ""), r#"{"tag":[{"system":"urn:t","code":"vip"}]}"#, "CamelFhir.id", id.clone()).unwrap();
    assert!(meta.get("tag").is_none());

    // validate: server $validate, local profile
    let ex = t.request(&ep("validate/resource", ""), |e| e.message.body = Value::from(r#"{"resourceType":"Patient","birthDate":"1970-1-1"}"#)).unwrap();
    assert_eq!(ex.message.headers.get("CamelFhirValidationSuccessful"), Some(&Value::Bool(false)));
    assert!(ex.message.body.to_json().contains("not a valid date"), "{}", ex.message.body.to_json());
    let ex = t.request(&ep("validate/resource", ""), |e| e.message.body = Value::from(r#"{"resourceType":"Patient","birthDate":"1970-01-01"}"#)).unwrap();
    assert_eq!(ex.message.headers.get("CamelFhirValidationSuccessful"), Some(&Value::Bool(true)));
    let ex = t.request(&ep("validate/resource", "&profile=#strictPatient"), |e| e.message.body = Value::from(r#"{"resourceType":"Patient","birthDate":"1970-01-01"}"#)).unwrap();
    assert_eq!(ex.message.headers.get("CamelFhirValidationSuccessful"), Some(&Value::Bool(false)));
    assert!(ex.message.body.to_json().contains("Patient.identifier: minimum required = 1, but only found 0"), "{}", ex.message.body.to_json());
}
