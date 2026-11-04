//! YAML `marshal: {protobuf: {...}}` with options resolves a configured protobuf data format
//! (registered by camel-grpc as a data format factory).

use camel_core::prelude::*;

#[test]
fn yaml_protobuf_marshal_unmarshal_with_options() {
    let proto = concat!(env!("CARGO_MANIFEST_DIR"), "/../camel-grpc/tests/resources/greeter.proto").replace('\\', "/");
    let main = camel_main::new_main();
    let ctx = main.context().clone();
    let yaml = format!(
        r#"
- from:
    uri: direct:proto
    steps:
      - marshal:
          protobuf:
            instanceClass: helloworld.HelloRequest
            protoFile: "{proto}"
      - to: mock:wire
      - unmarshal:
          protobuf:
            instanceClass: helloworld.HelloRequest
            protoFile: "{proto}"
      - to: mock:back
"#
    );
    camel_yaml::add_routes_from_yaml(&ctx, &yaml).unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:proto", Value::object([("name", "Camel")])).unwrap();
    let wire = ctx.mock_endpoint("mock:wire").unwrap().received_bodies();
    // field 1 (name), length-delimited: 0x0a, len 5, "Camel"
    assert_eq!(wire[0].to_bytes(), b"\x0a\x05Camel".to_vec());
    let back = ctx.mock_endpoint("mock:back").unwrap().received_bodies();
    assert_eq!(back[0].get("name"), Some(&Value::from("Camel")));
    ctx.stop().unwrap();
}
