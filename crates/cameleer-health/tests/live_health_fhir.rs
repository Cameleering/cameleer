//! Live FHIR R4 against the HAPI FHIR JPA server 8.12.0: the `fhir` component's create, read,
//! update, search, history and delete, and the fhirJson/fhirXml data formats on what HAPI returns.
//!
//! Needs `scripts/health-interop.sh all`, `eval "$(scripts/health-interop.sh env)"` and
//! `CAMEL_HEALTH_INTEROP=1`; returns early otherwise.

use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;

fn enabled() -> bool {
    let on = std::env::var("CAMEL_HEALTH_INTEROP").map(|v| v == "1").unwrap_or(false);
    if !on {
        eprintln!("skipped: set CAMEL_HEALTH_INTEROP=1 after `scripts/health-interop.sh all`");
    }
    on
}

fn base() -> String {
    std::env::var("CAMEL_HEALTH_FHIR_URL").unwrap_or_else(|_| "http://127.0.0.1:18085/fhir".into())
}

#[test]
fn live_fhir_component_against_hapi() {
    if !enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.add_pack(&cameleer_health::HEALTH);
    ctx.add_routes(|b| {
        b.from("direct:to-xml").marshal(DataFormatDefinition::new("fhirXml"));
        b.from("direct:from-xml").unmarshal(DataFormatDefinition::new("fhirXml"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let ep = |m: &str, extra: &str| format!("fhir://{}?serverUrl={}&fhirVersion=R4{}", m, base(), extra);
    let family = format!("Camel{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    let patient = format!(r#"{{"resourceType":"Patient","name":[{{"family":"{}","given":["Jane"]}}],"gender":"female","birthDate":"1970-01-01"}}"#, family);
    // create
    let created = t.request_body(&ep("create/resource", ""), patient).unwrap();
    let id = created.get("id").map(|v| v.to_string()).expect("HAPI assigned an id");
    assert_eq!(created.get("meta").and_then(|m| m.get("versionId")), Some(&Value::from("1")));
    // read, as JSON and as XML
    let read = t.request_body_and_header(&ep("read/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    assert_eq!(read.get("birthDate"), Some(&Value::from("1970-01-01")));
    let x = t.request_body(&ep("read/resourceById", &format!("&encoding=XML&stringId=Patient/{}", id)), Value::Null).unwrap();
    assert_eq!(x.get("gender"), Some(&Value::from("female")));
    // update
    let mut changed = read.clone();
    if let Value::Object(m) = &mut changed {
        m.insert("gender".into(), Value::from("other"));
    }
    let updated = t.request_body(&ep("update/resource", ""), changed).unwrap();
    assert_eq!(updated.get("meta").and_then(|m| m.get("versionId")), Some(&Value::from("2")));
    // search
    let bundle = t.request_body(&ep("search/searchByUrl", ""), format!("Patient?family={}", family)).unwrap();
    assert_eq!(bundle.get("type"), Some(&Value::from("searchset")));
    assert_eq!(bundle.get("total"), Some(&Value::Int(1)));
    // history
    let hist = t.request_body_and_header(&ep("history/onInstance", ""), Value::Null, "CamelFhir.id", format!("Patient/{}", id)).unwrap();
    assert_eq!(hist.get("type"), Some(&Value::from("history")));
    // HAPI's JSON through fhirXml and back keeps the resource
    let xml = t.request_body("direct:to-xml", updated.clone()).unwrap().to_string();
    assert!(xml.contains("<Patient xmlns=\"http://hl7.org/fhir\">") && xml.contains(&family), "{xml}");
    let back = t.request_body("direct:from-xml", xml).unwrap();
    assert_eq!(back.get("gender"), Some(&Value::from("other")));
    assert_eq!(back.get("name"), updated.get("name"));
    // delete; reading it again fails with ResourceGoneException (HTTP 410)
    t.request_body_and_header(&ep("delete/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap();
    let gone = t.request_body_and_header(&ep("read/resourceById", "&resourceClass=Patient"), Value::Null, "CamelFhir.id", id.clone()).unwrap_err();
    assert!(gone.chain().any(|e| e.kind == "ResourceGoneException" || e.kind == "ResourceNotFoundException"), "{gone:?}");
    ctx.stop().unwrap();
}
