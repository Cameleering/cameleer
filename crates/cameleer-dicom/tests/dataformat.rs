//! The dicom and dicomJson data formats, the dicom language and ${dicom.*} in Simple, in routes.

use cameleer_core::dataformat::DataFormatDefinition;
use cameleer_core::prelude::*;
use cameleer_dicom::testing::Instance;
use cameleer_dicom::{ts, DicomFile, ReadOptions};

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_dicom::register(&ctx);
    ctx
}

#[test]
fn unmarshal_marshal_is_byte_exact_and_sets_headers() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:rt").unmarshal(DataFormatDefinition::new("dicom")).marshal(DataFormatDefinition::new("dicom"));
        b.from("direct:tree").unmarshal(DataFormatDefinition::new("dicom"));
        b.from("direct:implicit").marshal(DataFormatDefinition::new("dicom").option("transferSyntax", ts::IMPLICIT_VR_LITTLE_ENDIAN));
        b.from("direct:jpeg").marshal(DataFormatDefinition::new("dicom").option("transferSyntax", ts::JPEG_BASELINE));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for syntax in [ts::EXPLICIT_VR_LITTLE_ENDIAN, ts::IMPLICIT_VR_LITTLE_ENDIAN, ts::DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN, ts::EXPLICIT_VR_BIG_ENDIAN] {
        let bytes = Instance::ct("P1", 1, 1, 1).part10_in(syntax);
        let out = t.request_body("direct:rt", Value::Bytes(bytes.clone())).unwrap();
        assert_eq!(out, Value::Bytes(bytes), "{syntax}");
    }
    let mut ex = ctx.create_exchange();
    ex.set_body(Value::Bytes(Instance::ct("P1", 1, 2, 3).part10()));
    let ex = t.send("direct:tree", ex);
    assert!(ex.exception.is_none());
    assert_eq!(ex.header("CamelDicomSopInstanceUid").unwrap().to_string(), "1.2.826.0.1.3680043.10.1234.1.2.3");
    assert_eq!(ex.header("CamelDicomTransferSyntaxUid").unwrap().to_string(), ts::EXPLICIT_VR_LITTLE_ENDIAN);
    assert_eq!(ex.body().get("00100020").unwrap().get("Value").unwrap().as_array().unwrap()[0], Value::from("P1"));
    // transcoding between native syntaxes
    let implicit = t.request_body("direct:implicit", Value::Bytes(Instance::ct("P1", 1, 1, 1).part10())).unwrap();
    let Value::Bytes(b) = implicit else { panic!() };
    let f = DicomFile::parse(&b, ReadOptions::default()).unwrap();
    assert_eq!(f.transfer_syntax_uid(), ts::IMPLICIT_VR_LITTLE_ENDIAN);
    assert_eq!(f.dataset, Instance::ct("P1", 1, 1, 1).dataset());
    // but not to a compressed one
    let e = t.request_body("direct:jpeg", Value::Bytes(Instance::ct("P1", 1, 1, 1).part10())).unwrap_err();
    assert!(e.chain().any(|x| x.kind == "DicomTranscodeException"), "{e:?}");
    ctx.stop().unwrap();
}

#[test]
fn simple_and_dicom_language_route_on_tags() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:route")
            .choice()
            .when(simple("${dicom.Modality} == 'CT' && ${dicom.(0010,0020)} startsWith 'VIP'"))
            .to("mock:vip")
            .when(language("dicom", "PatientID").is_equal_to("ROUTINE"))
            .to("mock:routine")
            .otherwise()
            .to("mock:other")
            .end();
        b.from("direct:text").set_body(simple("${dicom.PatientName} ${dicom.ImageType[2]} ${dicom.StudyInstanceUID}"));
        b.from("direct:tree").unmarshal(DataFormatDefinition::new("dicom")).set_body(simple("${dicom.PatientID}/${dicom.TransferSyntaxUID}"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let vip = ctx.mock_endpoint("mock:vip").unwrap();
    let routine = ctx.mock_endpoint("mock:routine").unwrap();
    vip.expected_message_count(1);
    routine.expected_message_count(1);
    t.send_body("direct:route", Value::Bytes(Instance::ct("VIP-1", 1, 1, 1).part10())).unwrap();
    t.send_body("direct:route", Value::Bytes(Instance::ct("ROUTINE", 1, 1, 1).part10())).unwrap();
    vip.assert_is_satisfied().unwrap();
    routine.assert_is_satisfied().unwrap();
    let text = t.request_body("direct:text", Value::Bytes(Instance::ct("P9", 4, 1, 1).part10())).unwrap();
    assert_eq!(text.to_string(), "Test^P9 AXIAL 1.2.826.0.1.3680043.10.1234.4");
    let text = t.request_body("direct:tree", Value::Bytes(Instance::ct("P9", 4, 1, 1).part10())).unwrap();
    assert_eq!(text.to_string(), format!("P9/{}", ts::EXPLICIT_VR_LITTLE_ENDIAN));
    // a body that is not DICOM
    assert!(t.request_body("direct:text", "hello").is_err());
    ctx.stop().unwrap();
}

#[test]
fn dicom_json_model() {
    let ctx = ctx();
    ctx.add_routes(|b| {
        b.from("direct:json").marshal(DataFormatDefinition::new("dicomJson").option("bulkDataThreshold", "8").option("bulkDataUri", "http://x/{sopInstanceUid}/{tag}"));
        b.from("direct:back").unmarshal(DataFormatDefinition::new("dicomJson")).marshal(DataFormatDefinition::new("dicom"));
        b.from("direct:all").marshal(DataFormatDefinition::new("dicomJson"));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let inst = Instance::ct("P1", 1, 1, 1);
    let json = t.request_body("direct:json", Value::Bytes(inst.part10())).unwrap().to_string();
    let v = cameleer_core::json::parse(&json).unwrap();
    assert_eq!(v.get("00100010").unwrap().get("Value").unwrap().as_array().unwrap()[0].get("Alphabetic"), Some(&Value::from("Test^P1")));
    assert_eq!(v.get("00280010").unwrap().get("Value").unwrap().as_array().unwrap()[0], Value::Int(4));
    assert_eq!(v.get("7FE00010").unwrap().get("BulkDataURI"), Some(&Value::from(format!("http://x/{}/7FE00010", inst.instance_uid))));
    assert!(v.get("00020010").is_none(), "no file meta in DICOM JSON");
    assert!(!json.contains("_raw") && !json.contains("_undefinedLength"));
    // JSON with inline binary converts back into the same data set
    let full = t.request_body("direct:all", Value::Bytes(inst.part10())).unwrap().to_string();
    let Value::Bytes(back) = t.request_body("direct:back", full).unwrap() else { panic!() };
    let f = DicomFile::parse(&back, ReadOptions::default()).unwrap();
    assert_eq!(f.dataset, inst.dataset());
    ctx.stop().unwrap();
}
