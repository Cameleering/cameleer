//! FHIR conversions and data formats, and the fhir component against the embedded mock server
//! (moved from cameleer-soap with FHIR itself).

use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;
use cameleer_health::fhir::json;
use cameleer_health::fhir::testing::FhirMock;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx
}

const PATIENT: &str = r#"{"resourceType":"Patient","id":"example","text":{"status":"generated","div":"<div xmlns=\"http://www.w3.org/1999/xhtml\"><table><tbody><tr><td>Name</td><td>Peter James <b>Chalmers</b> (\"Jim\")</td></tr></tbody></table></div>"},"identifier":[{"use":"usual","type":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/v2-0203","code":"MR"}]},"system":"urn:oid:1.2.36.146.595.217.0.1","value":"12345","period":{"start":"2001-05-06"},"assigner":{"display":"Acme Healthcare"}}],"active":true,"name":[{"use":"official","family":"Chalmers","given":["Peter","James"]},{"use":"usual","given":["Jim"]},{"use":"maiden","family":"Windsor","given":["Peter","James"],"period":{"end":"2002"}}],"telecom":[{"use":"home"},{"system":"phone","value":"(03) 5555 6473","use":"work","rank":1}],"gender":"male","birthDate":"1974-12-25","_birthDate":{"extension":[{"url":"http://hl7.org/fhir/StructureDefinition/patient-birthTime","valueDateTime":"1974-12-25T14:35:45-05:00"}]},"deceasedBoolean":false,"address":[{"use":"home","type":"both","text":"534 Erewhon St PeasantVille, Rainbow, Vic  3999","line":["534 Erewhon St"],"city":"PleasantVille","district":"Rainbow","state":"Vic","postalCode":"3999","period":{"start":"1974-12-25"}}],"contact":[{"relationship":[{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/v2-0131","code":"N"}]}],"name":{"family":"du Marché","_family":{"extension":[{"url":"http://hl7.org/fhir/StructureDefinition/humanname-own-prefix","valueString":"VV"}]},"given":["Bénédicte"]},"telecom":[{"system":"phone","value":"+33 (237) 998327"}],"gender":"female","period":{"start":"2012"}}],"managingOrganization":{"reference":"Organization/1"}}"#;

/// Abridged from the FHIR R4 `observation-example.xml` (body weight).
const OBSERVATION_XML: &str = r#"<Observation xmlns="http://hl7.org/fhir"><id value="example"/><status value="final"/><category><coding><system value="http://terminology.hl7.org/CodeSystem/observation-category"/><code value="vital-signs"/><display value="Vital Signs"/></coding></category><code><coding><system value="http://loinc.org"/><code value="29463-7"/><display value="Body Weight"/></coding><coding><system value="http://loinc.org"/><code value="3141-9"/></coding></code><subject><reference value="Patient/example"/></subject><effectiveDateTime value="2016-03-28"/><valueQuantity><value value="185.00"/><unit value="lbs"/><system value="http://unitsofmeasure.org"/><code value="[lb_av]"/></valueQuantity><component><code><text value="note"/></code><valueString value="fasting"/></component></Observation>"#;

#[test]
fn fhir_json_xml_round_trips_on_spec_examples() {
    use cameleer_health::fhir::{json_to_xml, xml_to_json};
    // JSON -> XML -> JSON
    let xml = json_to_xml(PATIENT, false).unwrap();
    assert!(xml.contains("<family value=\"du Marché\"><extension url=\"http://hl7.org/fhir/StructureDefinition/humanname-own-prefix\"><valueString value=\"VV\"/></extension></family>"), "{}", xml);
    assert!(xml.contains("<rank value=\"1\"/>"));
    assert!(xml.contains("<div xmlns=\"http://www.w3.org/1999/xhtml\"><table>"));
    let back = xml_to_json(&xml, false).unwrap();
    assert_eq!(json::parse(&back).unwrap(), json::parse(PATIENT).unwrap());
    // XML -> JSON -> XML
    let j = xml_to_json(OBSERVATION_XML, false).unwrap();
    let tree = json::parse(&j).unwrap();
    assert_eq!(tree.get("category").map(|c| matches!(c, json::J::Arr(_))), Some(true), "category repeats");
    assert_eq!(tree.get("valueQuantity").and_then(|q| q.get("value")), Some(&json::J::Num("185.00".into())), "decimal lexeme kept");
    assert!(j.contains(r#""component":[{"code":{"text":"note"},"valueString":"fasting"}]"#), "{}", j);
    assert_eq!(json_to_xml(&j, false).unwrap(), OBSERVATION_XML);
    // pretty printing parses back to the same tree
    let pretty = json_to_xml(&j, true).unwrap();
    assert_eq!(json::parse(&xml_to_json(&pretty, false).unwrap()).unwrap(), tree);
}

#[test]
fn fhir_data_formats() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:json2xml").unmarshal(DataFormatDefinition::new("fhirJson")).marshal(DataFormatDefinition::new("fhirXml"));
        b.from("direct:xml2json").marshal(DataFormatDefinition::new("fhirJson").option("suppressNarratives", true).option("dontEncodeElements", "Patient.contact"));
        b.from("direct:parse").unmarshal(DataFormatDefinition::new("fhirXml"));
        b.from("direct:only").marshal(DataFormatDefinition::new("fhirJson").option("encodeElements", "Patient.name"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let xml = t.request_body("direct:json2xml", PATIENT).unwrap().to_string();
    assert!(xml.starts_with("<Patient xmlns=\"http://hl7.org/fhir\"><id value=\"example\"/>"), "{}", xml);
    // marshal accepts FHIR XML text and converts it
    let j = t.request_body("direct:xml2json", xml.clone()).unwrap().to_string();
    let tree = json::parse(&j).unwrap();
    assert!(tree.get("text").is_none() && tree.get("contact").is_none() && tree.get("name").is_some());
    let v = t.request_body("direct:parse", OBSERVATION_XML).unwrap();
    assert_eq!(v.get("resourceType"), Some(&Value::from("Observation")));
    assert_eq!(v.get("valueQuantity").and_then(|q| q.get("value")), Some(&Value::from("185.00")));
    let only = t.request_body("direct:only", PATIENT).unwrap().to_string();
    assert_eq!(json::parse(&only).unwrap().get("gender"), None);
    // invalid content
    let e = t.request_body("direct:json2xml", "{\"name\":\"x\"}").unwrap_err();
    assert!(e.chain().any(|x| x.kind == "DataFormatException"), "{:?}", e);
    ctx.stop().unwrap();
}

#[test]
fn fhir_component_crud_search_history_transaction() {
    let mock = FhirMock::start();
    let ctx = ctx();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let base = format!("serverUrl={}&fhirVersion=R4", mock.url());
    let ep = |m: &str, extra: &str| format!("fhir://{}?{}{}", m, base, extra);

    let created = t.request_body(&ep("create/resource", ""), PATIENT).unwrap();
    let id = created.get("id").unwrap().to_string();
    assert_eq!(created.get("meta").and_then(|m| m.get("versionId")), Some(&Value::from("1")));
    assert_eq!(mock.count("Patient"), 1);

    let read = t.request_body_and_header(&ep("read/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(read.get("birthDate"), Some(&Value::from("1974-12-25")));

    let mut changed = read.clone();
    changed.as_object_mut().unwrap().insert("gender".into(), Value::from("other"));
    let updated = t.request_body(&ep("update/resource", ""), changed).unwrap();
    assert_eq!(updated.get("meta").and_then(|m| m.get("versionId")), Some(&Value::from("2")));

    // XML encoding against the same server
    let x = t.request_body(&ep("read/resourceById", &format!("&encoding=XML&stringId=Patient/{}", id)), Value::Null).unwrap();
    assert_eq!(x.get("gender"), Some(&Value::from("other")));
    assert!(mock.requests().iter().any(|r| r.starts_with("GET /fhir/Patient/")));

    let obs = cameleer_health::fhir::xml_to_json(OBSERVATION_XML, false).unwrap().replace("Patient/example", &format!("Patient/{}", id));
    t.request_body(&ep("create/resource", "&encoding=XML"), obs).unwrap();

    let bundle = t.request_body(&ep("search/searchByUrl", ""), "Patient?family=chal").unwrap();
    assert_eq!(bundle.get("type"), Some(&Value::from("searchset")));
    assert_eq!(bundle.get("total"), Some(&Value::Int(1)));
    let none = t.request_body(&ep("search/searchByUrl", ""), "Patient?family=Nobody").unwrap();
    assert_eq!(none.get("total"), Some(&Value::Int(0)));
    let by_subject = t.request_body(&ep("search/searchByUrl", ""), format!("Observation?subject=Patient/{}&code=http://loinc.org|29463-7", id)).unwrap();
    assert_eq!(by_subject.get("total"), Some(&Value::Int(1)));

    let hist = t.request_body_and_header(&ep("history/onInstance", ""), Value::Null, "CamelFhir.id", format!("Patient/{}", id)).unwrap();
    assert_eq!(hist.get("entry").unwrap().as_array().unwrap().len(), 2);
    let type_hist = t.request_body_and_header(&ep("history/onType", ""), Value::Null, "CamelFhir.resourceType", "Observation").unwrap();
    assert_eq!(type_hist.get("total"), Some(&Value::Int(1)));

    let caps = t.request_body(&ep("capabilities/ofType", ""), Value::Null).unwrap();
    assert_eq!(caps.get("resourceType"), Some(&Value::from("CapabilityStatement")));

    // transaction with an internal urn:uuid reference
    let tx = r#"{"resourceType":"Bundle","type":"transaction","entry":[
        {"fullUrl":"urn:uuid:61ebe359-bfdc-4613-8bf2-c5e300945f0a","resource":{"resourceType":"Organization","name":"Acme"},"request":{"method":"POST","url":"Organization"}},
        {"resource":{"resourceType":"Practitioner","name":[{"family":"Careful"}],"identifier":[{"value":"p1"}]},"request":{"method":"POST","url":"Practitioner"}},
        {"resource":{"resourceType":"Patient","id":"tx1","managingOrganization":{"reference":"urn:uuid:61ebe359-bfdc-4613-8bf2-c5e300945f0a"}},"request":{"method":"PUT","url":"Patient/tx1"}}]}"#;
    let resp = t.request_body(&ep("transaction/withBundle", ""), tx).unwrap();
    assert_eq!(resp.get("type"), Some(&Value::from("transaction-response")));
    let entries = resp.get("entry").unwrap().as_array().unwrap();
    assert_eq!(entries[0].get("response").and_then(|r| r.get("status")), Some(&Value::from("201 Created")));
    let p = mock.resource_json("Patient", "tx1").unwrap();
    assert!(p.contains("\"reference\":\"Organization/"), "{}", p);
    let list = t.request_body(&ep("transaction/withResources", ""), Value::Array(vec![Value::object([("resourceType", "Organization"), ("name", "Beta")])])).unwrap();
    assert_eq!(list.as_array().unwrap()[0].get("name"), Some(&Value::from("Beta")));

    // paging
    let page1 = t.request_body(&ep("search/searchByUrl", ""), "Organization?_count=1").unwrap();
    assert_eq!(page1.get("total"), Some(&Value::Int(2)));
    let page2 = t.request_body(&ep("load-page/next", ""), page1).unwrap();
    assert_eq!(page2.get("entry").unwrap().as_array().unwrap()[0].get("resource").and_then(|r| r.get("name")), Some(&Value::from("Beta")));

    // delete, then read -> gone; unknown -> not found
    t.request_body_and_header(&ep("delete/resourceById", "&type=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    let e = t.request_body_and_header(&ep("read/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap_err();
    assert!(e.chain().any(|x| x.kind == "ResourceGoneException"), "{:?}", e);
    let e = t.request_body_and_header(&ep("read/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", "999").unwrap_err();
    let nf = e.chain().find(|x| x.kind == "ResourceNotFoundException").expect("not found");
    assert!(nf.message.contains("Patient/999 is not known"), "{}", nf.message);
    assert!(nf.is_a("BaseServerResponseException"));
    t.request_body(&ep("delete/resourceConditionalByUrl", ""), "Organization?name=Beta").unwrap();
    assert_eq!(mock.count("Organization"), 1);
    ctx.stop().unwrap();
}

#[test]
fn fhir_component_authentication() {
    let basic = FhirMock::start_with_basic("camel", "s3cret");
    let bearer = FhirMock::start_with_bearer("tok-123");
    let ctx = ctx();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let caps = |url: &str, auth: &str| t.request_body(&format!("fhir://capabilities/ofType?serverUrl={}{}", url, auth), Value::Null);
    assert!(caps(&basic.url(), "&username=camel&password=s3cret").is_ok());
    assert!(caps(&bearer.url(), "&accessToken=tok-123").is_ok());
    let e = caps(&basic.url(), "&username=camel&password=nope").unwrap_err();
    assert!(e.chain().any(|x| x.kind == "AuthenticationException"), "{:?}", e);
    assert!(caps(&bearer.url(), "").is_err());
    // bad options
    assert!(ctx.get_endpoint("fhir://nope/what?serverUrl=http://x").is_err());
    assert!(ctx.get_endpoint("fhir://create/resource?serverUrl=http://x&fhirVersion=DSTU1").is_err());
    ctx.stop().unwrap();
}
