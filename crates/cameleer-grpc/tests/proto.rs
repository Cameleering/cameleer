//! .proto parser and dynamic codec tests (wire bytes computed by hand from the
//! protobuf encoding documentation).

use cameleer_core::value::Value;
use cameleer_grpc::proto::{DescriptorPool, FieldType, Label};

const ENCODING_PROTO: &str = r#"
// The examples of https://protobuf.dev/programming-guides/encoding/
syntax = "proto3";
package enc;

message Test1 { int32 a = 1; }
message Test2 { string b = 2; }
message Test3 { Test1 c = 3; }
message Test4 { string d = 1; repeated int32 e = 2; }
message Signed { sint32 s32 = 1; sint64 s64 = 2; int32 i32 = 3; int64 i64 = 4; }
message Fixed { fixed32 f32 = 1; sfixed64 sf64 = 2; float fl = 3; double db = 4; bool ok = 5; bytes raw = 6; uint64 big = 7; }
message Unpacked { repeated int32 v = 1 [packed = false]; }
"#;

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn obj(items: Vec<(&str, Value)>) -> Value {
    Value::object(items)
}

#[test]
fn classic_wire_examples() {
    let pool = DescriptorPool::from_source("encoding.proto", ENCODING_PROTO).unwrap();
    // 150 -> 08 96 01
    let t1 = obj(vec![("a", Value::Int(150))]);
    assert_eq!(pool.encode("enc.Test1", &t1).unwrap(), hex("08 96 01"));
    assert_eq!(pool.decode("enc.Test1", &hex("08 96 01")).unwrap(), t1);
    // string "testing" -> 12 07 74 65 73 74 69 6e 67
    let t2 = obj(vec![("b", Value::from("testing"))]);
    assert_eq!(pool.encode("enc.Test2", &t2).unwrap(), hex("12 07 74 65 73 74 69 6e 67"));
    assert_eq!(pool.decode("enc.Test2", &hex("12 07 74 65 73 74 69 6e 67")).unwrap(), t2);
    // embedded message -> 1a 03 08 96 01
    let t3 = obj(vec![("c", t1.clone())]);
    assert_eq!(pool.encode("enc.Test3", &t3).unwrap(), hex("1a 03 08 96 01"));
    assert_eq!(pool.decode("enc.Test3", &hex("1a 03 08 96 01")).unwrap(), t3);
    // packed repeated -> 0a 05 68 65 6c 6c 6f 12 07 01 02 03 8e 02 9e a7 05 ... (d="hello", e=[1,2,3,270,86942])
    let t4 = obj(vec![("d", Value::from("hello")), ("e", Value::Array(vec![1.into(), 2.into(), 3.into(), 270.into(), 86942.into()]))]);
    let bytes = pool.encode("enc.Test4", &t4).unwrap();
    assert_eq!(bytes, hex("0a 05 68 65 6c 6c 6f 12 08 01 02 03 8e 02 9e a7 05"));
    assert_eq!(pool.decode("enc.Test4", &bytes).unwrap(), t4);
    // unpacked encoding of the same repeated field is accepted when decoding
    let unpacked = hex("10 01 10 02 10 03 10 8e 02 10 9e a7 05");
    assert_eq!(pool.decode("enc.Test4", &unpacked).unwrap().get("e"), t4.get("e"));
    // proto3 defaults: absent fields decode to defaults, default values are not encoded
    assert_eq!(pool.decode("enc.Test1", &[]).unwrap(), obj(vec![("a", Value::Int(0))]));
    assert_eq!(pool.encode("enc.Test1", &obj(vec![("a", Value::Int(0))])).unwrap(), Vec::<u8>::new());
    assert_eq!(pool.decode("enc.Test4", &[]).unwrap(), obj(vec![("d", Value::from("")), ("e", Value::Array(vec![]))]));
}

#[test]
fn signed_fixed_and_scalars() {
    let pool = DescriptorPool::from_source("encoding.proto", ENCODING_PROTO).unwrap();
    let s = obj(vec![("s32", Value::Int(-1)), ("s64", Value::Int(-2)), ("i32", Value::Int(-1)), ("i64", Value::Int(1))]);
    let b = pool.encode("enc.Signed", &s).unwrap();
    // sint32 -1 -> zigzag 1; sint64 -2 -> 3; int32 -1 -> ten byte varint; int64 1
    assert_eq!(b, hex("08 01 10 03 18 ff ff ff ff ff ff ff ff ff 01 20 01"));
    assert_eq!(pool.decode("enc.Signed", &b).unwrap(), s);

    let f = obj(vec![
        ("f32", Value::Int(1)),
        ("sf64", Value::Int(-2)),
        ("fl", Value::Float(1.5)),
        ("db", Value::Float(-0.25)),
        ("ok", Value::Bool(true)),
        ("raw", Value::Bytes(vec![0, 255])),
        ("big", Value::String("18446744073709551615".into())),
    ]);
    let b = pool.encode("enc.Fixed", &f).unwrap();
    assert_eq!(
        b,
        hex("0d 01 00 00 00  11 fe ff ff ff ff ff ff ff  1d 00 00 c0 3f  21 00 00 00 00 00 00 d0 bf  28 01  32 02 00 ff  38 ff ff ff ff ff ff ff ff ff 01")
    );
    assert_eq!(pool.decode("enc.Fixed", &b).unwrap(), f);

    let u = obj(vec![("v", Value::Array(vec![1.into(), 2.into()]))]);
    assert_eq!(pool.encode("enc.Unpacked", &u).unwrap(), hex("08 01 08 02"));
    // range checks and unknown fields are errors on encode
    assert!(pool.encode("enc.Signed", &obj(vec![("i32", Value::Int(1 << 40))])).is_err());
    assert!(pool.encode("enc.Test1", &obj(vec![("nope", Value::Int(1))])).is_err());
    // coercions: strings and floats to ints
    assert_eq!(pool.encode("enc.Test1", &obj(vec![("a", Value::from("150"))])).unwrap(), hex("08 96 01"));
    // unknown fields are skipped on decode (field 9 varint, field 10 bytes, field 11 fixed32)
    let with_unknown = hex("48 05 52 02 01 02 5d 01 02 03 04 08 96 01");
    assert_eq!(pool.decode("enc.Test1", &with_unknown).unwrap(), obj(vec![("a", Value::Int(150))]));
    // truncated input
    assert!(pool.decode("enc.Test1", &hex("08 96")).is_err());
    // wire type mismatch (field 1 as length-delimited)
    assert!(pool.decode("enc.Test1", &hex("0a 01 00")).is_err());
}

const FULL_PROTO: &str = r#"
syntax = "proto3";

package demo.v1;

import "google/protobuf/timestamp.proto";
import "google/protobuf/empty.proto";
import "google/protobuf/struct.proto";
import "google/protobuf/wrappers.proto";

option java_multiple_files = true;
option java_package = "org.example.demo";

/* a block comment */
message Person {
  string name = 1;
  int32 id = 2 [json_name = "personId"];
  string email_address = 3;

  enum PhoneType {
    option allow_alias = true;
    MOBILE = 0;
    HOME = 1;
    WORK = 2;
    CELL = 0;
  }

  message PhoneNumber {
    string number = 1;
    PhoneType type = 2;
  }

  repeated PhoneNumber phones = 4;
  map<string, int32> scores = 5;
  map<int32, PhoneNumber> by_id = 6;
  oneof contact {
    string twitter = 7;
    PhoneNumber pager = 8;
  }
  google.protobuf.Timestamp last_updated = 9;
  optional int32 age = 10;
  repeated PhoneType kinds = 11;
  google.protobuf.Struct extra = 12;
  google.protobuf.StringValue nickname = 13;
  reserved 20 to 25;
  reserved "old";
}

message AddressBook { repeated Person people = 1; }

service Directory {
  option deprecated = false;
  rpc Get (Person) returns (Person);
  rpc List (google.protobuf.Empty) returns (stream Person) {}
  rpc Upload (stream Person) returns (AddressBook) { option deprecated = true; }
  rpc Chat (stream Person) returns (stream Person);
}
"#;

#[test]
fn parser_covers_proto3_constructs() {
    let pool = DescriptorPool::from_source("demo.proto", FULL_PROTO).unwrap();
    let p = pool.message("demo.v1.Person").unwrap();
    assert_eq!(p.field("name").unwrap().number, 1);
    assert_eq!(p.field("id").unwrap().json_name, "personId");
    assert_eq!(p.field("email_address").unwrap().json_name, "emailAddress");
    assert_eq!(p.field("phones").unwrap().label, Label::Repeated);
    assert_eq!(p.field("phones").unwrap().type_name, "demo.v1.Person.PhoneNumber");
    assert!(p.field("scores").unwrap().is_map);
    assert_eq!(p.field("twitter").unwrap().oneof_index, Some(0));
    assert_eq!(p.oneofs, vec!["contact".to_string()]);
    assert_eq!(p.field("last_updated").unwrap().type_name, "google.protobuf.Timestamp");
    assert_eq!(p.field("age").unwrap().label, Label::Optional);
    assert_eq!(p.field("kinds").unwrap().field_type, FieldType::Enum);
    assert_eq!(p.field("kinds").unwrap().packed, Some(true));
    let e = pool.enum_type("demo.v1.Person.PhoneType").unwrap();
    assert_eq!(e.number_of("WORK"), Some(2));
    let svc = pool.service("demo.v1.Directory").unwrap();
    let kinds: Vec<_> = svc.methods.iter().map(|m| (m.name.as_str(), m.kind())).collect();
    assert_eq!(kinds, vec![("Get", "unary"), ("List", "server_streaming"), ("Upload", "client_streaming"), ("Chat", "bidi_streaming")]);
    assert_eq!(svc.method("list").unwrap().input_type, "google.protobuf.Empty");
    assert_eq!(svc.method("Upload").unwrap().output_type, "demo.v1.AddressBook");
    // simple-name lookups
    assert!(pool.message("AddressBook").is_some());
    assert!(pool.service("Directory").is_some());
}

#[test]
fn round_trip_complex_message() {
    let pool = DescriptorPool::from_source("demo.proto", FULL_PROTO).unwrap();
    let person = obj(vec![
        ("name", Value::from("Ada")),
        ("id", Value::Int(7)),
        ("email_address", Value::from("ada@example.com")),
        ("phones", Value::Array(vec![obj(vec![("number", "555".into()), ("type", "WORK".into())]), obj(vec![("number", "556".into()), ("type", "MOBILE".into())])])),
        ("scores", obj(vec![("math", Value::Int(10)), ("art", Value::Int(-3))])),
        ("by_id", obj(vec![("42", obj(vec![("number", "1".into()), ("type", "HOME".into())]))])),
        ("pager", obj(vec![("number", "999".into()), ("type", "MOBILE".into())])),
        ("last_updated", obj(vec![("seconds", Value::Int(1_700_000_000)), ("nanos", Value::Int(5))])),
        ("age", Value::Int(0)),
        ("kinds", Value::Array(vec!["HOME".into(), "WORK".into()])),
        ("extra", obj(vec![("k", Value::from("v")), ("n", Value::Int(3)), ("f", Value::Float(1.5)), ("list", Value::Array(vec![Value::Bool(true), Value::Null])), ("o", obj(vec![("x", Value::Int(1))]))])),
        ("nickname", Value::from("countess")),
    ]);
    let bytes = pool.encode("demo.v1.Person", &person).unwrap();
    let back = pool.decode("demo.v1.Person", &bytes).unwrap();
    assert_eq!(back, person);
    // explicit presence: optional age=0 is encoded (field 10 varint 0 = 50 00)
    assert!(bytes.windows(2).any(|w| w == [0x50, 0x00]));

    // oneof: last one wins
    let mut both = pool.encode("demo.v1.Person", &obj(vec![("twitter", "@ada".into())])).unwrap();
    both.extend(pool.encode("demo.v1.Person", &obj(vec![("pager", obj(vec![("number", "1".into())]))])).unwrap());
    let d = pool.decode("demo.v1.Person", &both).unwrap();
    assert!(d.get("twitter").is_none());
    assert_eq!(d.get("pager").unwrap().get("number"), Some(&Value::from("1")));
    // message fields merge across occurrences
    let mut merged = pool.encode("demo.v1.Person", &obj(vec![("pager", obj(vec![("number", "1".into())]))])).unwrap();
    merged.extend(pool.encode("demo.v1.Person", &obj(vec![("pager", obj(vec![("type", "WORK".into())]))])).unwrap());
    let d = pool.decode("demo.v1.Person", &merged).unwrap();
    assert_eq!(d.get("pager").unwrap(), &obj(vec![("number", "1".into()), ("type", "WORK".into())]));
    // enums accept numbers; unknown enum numbers decode as ints
    let b = pool.encode("demo.v1.Person.PhoneNumber", &obj(vec![("type", Value::Int(2))])).unwrap();
    assert_eq!(b, vec![0x10, 0x02]);
    assert_eq!(pool.decode("demo.v1.Person.PhoneNumber", &[0x10, 0x09]).unwrap().get("type"), Some(&Value::Int(9)));
    assert!(pool.encode("demo.v1.Person.PhoneNumber", &obj(vec![("type", "FAX".into())])).is_err());
    // well-known Timestamp from an RFC 3339 string
    let b = pool.encode("demo.v1.Person", &obj(vec![("last_updated", "2023-11-14T22:13:20Z".into())])).unwrap();
    assert_eq!(pool.decode("demo.v1.Person", &b).unwrap().get("last_updated").unwrap().get("seconds"), Some(&Value::Int(1_700_000_000)));
    // json names accepted on input
    assert_eq!(pool.encode("demo.v1.Person", &obj(vec![("personId", Value::Int(150))])).unwrap(), hex("10 96 01"));
}

#[test]
fn json_mapping() {
    let pool = DescriptorPool::from_source("demo.proto", FULL_PROTO).unwrap();
    let native = obj(vec![
        ("email_address", Value::from("a@b")),
        ("id", Value::Int(3)),
        ("last_updated", obj(vec![("seconds", Value::Int(0)), ("nanos", Value::Int(21_000_000))])),
        ("nickname", Value::from("n")),
        ("phones", Value::Array(vec![obj(vec![("type", Value::Int(1))])])),
    ]);
    let json = pool.to_json("demo.v1.Person", &native).unwrap();
    assert_eq!(json.get("emailAddress"), Some(&Value::from("a@b")));
    assert_eq!(json.get("personId"), Some(&Value::Int(3)));
    assert_eq!(json.get("lastUpdated"), Some(&Value::from("1970-01-01T00:00:00.021Z")));
    assert_eq!(json.get("nickname"), Some(&Value::from("n")));
    assert_eq!(json.get("phones").unwrap().as_array().unwrap()[0].get("type"), Some(&Value::from("HOME")));
    let back = pool.from_json("demo.v1.Person", &json).unwrap();
    assert_eq!(back.get("email_address"), Some(&Value::from("a@b")));
    assert_eq!(back.get("last_updated").unwrap().get("nanos"), Some(&Value::Int(21_000_000)));
    // encode(from_json(x)) == encode(native)
    assert_eq!(pool.encode("demo.v1.Person", &back).unwrap(), pool.encode("demo.v1.Person", &native).unwrap());

    let enc = DescriptorPool::from_source("encoding.proto", ENCODING_PROTO).unwrap();
    let f = obj(vec![("sf64", Value::Int(-2)), ("raw", Value::Bytes(b"hi".to_vec())), ("db", Value::Float(f64::INFINITY))]);
    let j = enc.to_json("enc.Fixed", &f).unwrap();
    assert_eq!(j.get("sf64"), Some(&Value::from("-2")));
    assert_eq!(j.get("raw"), Some(&Value::from("aGk=")));
    assert_eq!(j.get("db"), Some(&Value::from("Infinity")));
    let n = enc.from_json("enc.Fixed", &j).unwrap();
    assert_eq!(n.get("sf64"), Some(&Value::Int(-2)));
    assert_eq!(n.get("raw"), Some(&Value::Bytes(b"hi".to_vec())));
}

#[test]
fn proto2_and_imports_from_directory() {
    let dir = std::env::temp_dir().join(format!("cameleer-grpc-proto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("common.proto"),
        r#"syntax = "proto2";
        package common;
        message Money { required string currency = 1; optional int64 units = 2 [default = 0]; repeated int32 tags = 3 [packed = true]; }
        enum Level { LOW = 1; HIGH = 2; }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("order.proto"),
        r#"syntax = "proto3";
        package shop;
        import "common.proto";
        message Order { string id = 1; common.Money total = 2; repeated common.Money parts = 3; common.Level level = 4; }"#,
    )
    .unwrap();
    let pool = DescriptorPool::from_file(dir.join("order.proto")).unwrap();
    assert_eq!(pool.files.len(), 2);
    let money = pool.message("common.Money").unwrap();
    assert_eq!(money.field("currency").unwrap().label, Label::Required);
    // proto2: optional fields have presence; repeated not packed unless asked
    let m = obj(vec![("currency", "EUR".into()), ("units", Value::Int(0)), ("tags", Value::Array(vec![1.into(), 2.into()]))]);
    let b = pool.encode("common.Money", &m).unwrap();
    assert_eq!(b, hex("0a 03 45 55 52 10 00 1a 02 01 02"));
    assert_eq!(pool.decode("common.Money", &b).unwrap(), m);
    // proto2 absent optional fields stay absent
    assert_eq!(pool.decode("common.Money", &hex("0a 03 45 55 52")).unwrap(), obj(vec![("currency", "EUR".into()), ("tags", Value::Array(vec![]))]));
    let order = obj(vec![("id", "o1".into()), ("total", obj(vec![("currency", "USD".into()), ("tags", Value::Array(vec![]))])), ("level", "HIGH".into())]);
    let b = pool.encode("shop.Order", &order).unwrap();
    let back = pool.decode("shop.Order", &b).unwrap();
    assert_eq!(back.get("total"), order.get("total"));
    assert_eq!(back.get("level"), Some(&Value::from("HIGH")));
    // missing import is reported
    std::fs::write(dir.join("broken.proto"), "syntax = \"proto3\"; import \"missing.proto\";").unwrap();
    let e = DescriptorPool::from_file(dir.join("broken.proto")).unwrap_err();
    assert!(e.message.contains("missing.proto"), "{}", e);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_errors_have_line_numbers() {
    let e = DescriptorPool::from_source("bad.proto", "syntax = \"proto3\";\nmessage A {\n  int32 a = ;\n}").unwrap_err();
    assert_eq!(e.line, 3);
    let e = DescriptorPool::from_source("bad.proto", "syntax = \"proto3\";\nmessage A { Unknown u = 1; }").unwrap_err();
    assert!(e.message.contains("unknown type"), "{}", e);
    let e = DescriptorPool::from_source("bad.proto", "syntax = \"proto4\";").unwrap_err();
    assert!(e.message.contains("proto4"));
    assert!(DescriptorPool::from_source("bad.proto", "message A { int32 a = 1; ").is_err());
}
