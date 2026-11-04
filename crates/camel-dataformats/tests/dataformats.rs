//! camel-dataformats: interoperability vectors and routes using every format through
//! `marshal`/`unmarshal` (Rust DSL, YAML DSL and the `dataformat:` component).

use camel_core::dataformat::DataFormatDefinition;
use camel_core::json;
use camel_core::prelude::*;
use camel_dataformats::{avro, bindy, cbor, pgp, tar};

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn ctx() -> CamelContext {
    let c = CamelContext::new();
    camel_dataformats::register(&c);
    c
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cdf-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ----------------------------------------------------------------------------- CBOR

#[test]
fn cbor_rfc8949_appendix_a() {
    // (hex, JSON of the decoded value, whether our preferred encoding reproduces it)
    let cases: &[(&str, &str, bool)] = &[
        ("00", "0", true),
        ("01", "1", true),
        ("0a", "10", true),
        ("17", "23", true),
        ("1818", "24", true),
        ("1819", "25", true),
        ("1864", "100", true),
        ("1903e8", "1000", true),
        ("1a000f4240", "1000000", true),
        ("1b000000e8d4a51000", "1000000000000", true),
        ("1bffffffffffffffff", "\"18446744073709551615\"", false),
        ("c249010000000000000000", "\"18446744073709551616\"", false),
        ("3bffffffffffffffff", "\"-18446744073709551616\"", false),
        ("c349010000000000000000", "\"-18446744073709551617\"", false),
        ("20", "-1", true),
        ("29", "-10", true),
        ("3863", "-100", true),
        ("3903e7", "-1000", true),
        ("f90000", "0.0", true),
        ("f93c00", "1.0", true),
        ("fb3ff199999999999a", "1.1", true),
        ("f93e00", "1.5", true),
        ("f97bff", "65504.0", true),
        ("fa47c35000", "100000.0", true),
        ("fa7f7fffff", "3.4028234663852886e38", true),
        ("fb7e37e43c8800759c", "1e300", true),
        ("f90001", "5.960464477539063e-8", true),
        ("f90400", "0.00006103515625", true),
        ("f9c400", "-4.0", true),
        ("fbc010666666666666", "-4.1", true),
        ("fa47c35000", "100000.0", true),
        ("f4", "false", true),
        ("f5", "true", true),
        ("f6", "null", true),
        ("f7", "null", false),
        ("f0", "16", false),
        ("f8ff", "255", false),
        ("c074323031332d30332d32315432303a30343a30305a", "\"2013-03-21T20:04:00Z\"", false),
        ("c11a514b67b0", "1363896240", false),
        ("c1fb41d452d9ec200000", "1363896240.5", false),
        ("d74401020304", "\"\\u0001\\u0002\\u0003\\u0004\"", false),
        ("6449455446", "\"IETF\"", true),
        ("60", "\"\"", true),
        ("6161", "\"a\"", true),
        ("62225c", r#""\"\\""#, true),
        ("62c3bc", "\"\u{fc}\"", true),
        ("63e6b0b4", "\"\u{6c34}\"", true),
        ("64f0908591", "\"\u{10151}\"", true),
        ("80", "[]", true),
        ("83010203", "[1,2,3]", true),
        ("8301820203820405", "[1,[2,3],[4,5]]", true),
        ("98190102030405060708090a0b0c0d0e0f101112131415161718181819", "[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25]", true),
        ("a0", "{}", true),
        ("a201020304", r#"{"1":2,"3":4}"#, false),
        ("a26161016162820203", r#"{"a":1,"b":[2,3]}"#, true),
        ("826161a161626163", r#"["a",{"b":"c"}]"#, true),
        ("a56161614161626142616361436164614461656145", r#"{"a":"A","b":"B","c":"C","d":"D","e":"E"}"#, true),
        ("7f657374726561646d696e67ff", "\"streaming\"", false),
        ("9fff", "[]", false),
        ("9f018202039f0405ffff", "[1,[2,3],[4,5]]", false),
        ("9f01820203820405ff", "[1,[2,3],[4,5]]", false),
        ("83018202039f0405ff", "[1,[2,3],[4,5]]", false),
        ("bf61610161629f0203ffff", r#"{"a":1,"b":[2,3]}"#, false),
        ("826161bf61626163ff", r#"["a",{"b":"c"}]"#, false),
        ("bf6346756ef563416d7421ff", r#"{"Fun":true,"Amt":-2}"#, false),
    ];
    for (h, j, enc) in cases {
        let v = cbor::decode(&unhex(h)).unwrap_or_else(|e| panic!("{}: {}", h, e));
        if h.starts_with("d744") {
            assert_eq!(v, Value::Bytes(vec![1, 2, 3, 4]));
            continue;
        }
        let expect = json::parse(j).unwrap();
        match (&v, &expect) {
            (Value::Float(a), Value::Float(b)) => assert!(a == b || (a.is_nan() && b.is_nan()), "{}: {} != {}", h, a, b),
            (Value::Float(a), Value::Int(b)) => assert_eq!(*a, *b as f64, "{}", h),
            _ => assert_eq!(v, expect, "{}", h),
        }
        if *enc {
            assert_eq!(hex(&cbor::encode(&v)), *h, "encode {}", j);
        }
    }
    // byte strings, special floats
    assert_eq!(cbor::decode(&unhex("4401020304")).unwrap(), Value::Bytes(vec![1, 2, 3, 4]));
    assert_eq!(cbor::decode(&unhex("5f42010243030405ff")).unwrap(), Value::Bytes(vec![1, 2, 3, 4, 5]));
    assert_eq!(hex(&cbor::encode(&Value::Bytes(vec![]))), "40");
    assert_eq!(hex(&cbor::encode(&Value::Float(-0.0))), "f98000");
    assert_eq!(hex(&cbor::encode(&Value::Float(f64::INFINITY))), "f97c00");
    assert_eq!(hex(&cbor::encode(&Value::Float(f64::NEG_INFINITY))), "f9fc00");
    assert_eq!(hex(&cbor::encode(&Value::Float(f64::NAN))), "f97e00");
    assert!(cbor::decode(&unhex("fa7f800000")).unwrap().as_f64().unwrap().is_infinite());
    assert!(cbor::decode(&unhex("fb7ff8000000000000")).unwrap().as_f64().unwrap().is_nan());
    assert!(cbor::decode(&unhex("8301")).is_err());
    assert!(cbor::decode(&unhex("0000")).is_err());
}

#[test]
fn cbor_route() {
    let c = ctx();
    c.add_routes(|b| {
        b.from("direct:cbor").unmarshal_json().marshal("cbor").to("direct:hex");
        b.from("direct:hex").unmarshal("cbor").marshal_json();
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let out = t.request_body("direct:cbor", r#"{"a":1,"b":[true,null,1.5,"x"]}"#).unwrap();
    assert_eq!(out.to_string(), r#"{"a":1,"b":[true,null,1.5,"x"]}"#);
}

// ----------------------------------------------------------------------------- Avro

const USER_SCHEMA: &str = r#"{"namespace":"example.avro","type":"record","name":"User","fields":[{"name":"name","type":"string"},{"name":"favorite_number","type":["int","null"]},{"name":"favorite_color","type":["string","null"]}]}"#;

#[test]
fn avro_hand_computed_bytes_and_routes() {
    // {"name":"Alyssa","favorite_number":256,"favorite_color":null}
    // name: 0c "Alyssa"; favorite_number: branch 0 (00), 256 -> zigzag 512 = 80 04; color: branch 1 (02)
    let s = avro::AvroSchema::parse(USER_SCHEMA).unwrap();
    let v = json::parse(r#"{"name":"Alyssa","favorite_number":256,"favorite_color":null}"#).unwrap();
    let b = avro::encode_binary(&s, &v).unwrap();
    assert_eq!(hex(&b), "0c416c7973736100800402");

    let c = ctx();
    c.add_routes(|r| {
        r.from("direct:avro")
            .unmarshal_json()
            .marshal(DataFormatDefinition::new("avro").option("schema", USER_SCHEMA))
            .to("mock:avro")
            .unmarshal(DataFormatDefinition::new("avro").option("schema", USER_SCHEMA))
            .marshal_json();
    })
    .unwrap();
    let yaml = format!(
        r#"
- from:
    uri: direct:avro-json
    steps:
      - marshal:
          avroJackson:
            schema: '{USER_SCHEMA}'
      - to: mock:avro-json
      - unmarshal:
          avro:
            schema: '{USER_SCHEMA}'
            encoding: json
            contentTypeHeader: false
- from:
    uri: direct:avro-jsonenc
    steps:
      - marshal:
          avro:
            schema: '{USER_SCHEMA}'
            encoding: json
"#
    );
    camel_yaml::add_routes_from_yaml(&c, &yaml).unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let out = t.request_body("direct:avro", r#"{"name":"Alyssa","favorite_number":256,"favorite_color":null}"#).unwrap();
    assert_eq!(out.to_string(), r#"{"name":"Alyssa","favorite_number":256,"favorite_color":null}"#);
    let m = c.mock_endpoint("mock:avro").unwrap().received_exchanges();
    assert_eq!(m[0].body(), &Value::Bytes(unhex("0c416c7973736100800402")));
    assert_eq!(m[0].header("Content-Type").unwrap().to_string(), "avro/binary");

    // avroJackson takes JSON text; the JSON encoding wraps union branches
    let enc = t.request_body("direct:avro-jsonenc", json::parse(r#"{"name":"Ben","favorite_number":7,"favorite_color":"red"}"#).unwrap()).unwrap();
    assert_eq!(enc.to_string(), r#"{"name":"Ben","favorite_number":{"int":7},"favorite_color":{"string":"red"}}"#);
    let err = t.request_body("direct:avro-json", enc.to_string());
    // binary produced by avroJackson cannot be read with the JSON encoding
    assert!(err.is_err());
    let bin = c.mock_endpoint("mock:avro-json").unwrap().received_exchanges()[0].body().clone();
    assert_eq!(avro::decode_binary(&s, &bin.to_bytes(), Default::default()).unwrap().get("favorite_color").unwrap().as_str(), Some("red"));
}

#[test]
fn avro_container_file_via_dataformat_component() {
    let d = tmpdir("avro");
    let schema_file = d.join("user.avsc");
    std::fs::write(&schema_file, USER_SCHEMA).unwrap();
    let c = ctx();
    let sf = schema_file.to_string_lossy().to_string();
    c.add_routes(move |r| {
        r.from("direct:write").unmarshal_json().to(format!("dataformat:avro:marshal?schemaFile={}&encoding=container&codec=deflate", sf).as_str());
        r.from("direct:read").to("dataformat:avro:unmarshal?encoding=container").marshal_json();
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let records = (0..50).map(|i| format!(r#"{{"name":"user{}","favorite_number":{},"favorite_color":null}}"#, i, i)).collect::<Vec<_>>().join(",");
    let file = t.request_body("direct:write", format!("[{}]", records)).unwrap();
    let bytes = file.to_bytes();
    assert!(avro::is_container(&bytes));
    let container = avro::read_container(&bytes, Default::default()).unwrap();
    assert_eq!(container.records.len(), 50);
    let back = t.request_body("direct:read", bytes).unwrap();
    assert_eq!(back.to_string(), format!("[{}]", records));
    let _ = std::fs::remove_dir_all(&d);
}

// ----------------------------------------------------------------------------- Bindy

#[test]
fn bindy_csv_yaml_descriptor() {
    let c = ctx();
    let yaml = r#"
- from:
    uri: direct:csv-in
    steps:
      - unmarshal:
          bindyCsv:
            unwrapSingleInstance: false
            descriptor:
              separator: ","
              skipFirstLine: true
              fields:
                - {name: orderNr, pos: 1, type: int, required: true}
                - {name: clientNr, pos: 2, trim: true}
                - {name: firstName, pos: 3}
                - {name: amount, pos: 5, type: decimal, precision: 2}
                - {name: orderDate, pos: 6, type: date, pattern: dd-MM-yyyy}
                - {name: currency, pos: 4, defaultValue: EUR}
      - marshal: {json: {}}
- from:
    uri: direct:csv-out
    steps:
      - unmarshal: {json: {}}
      - marshal:
          bindyCsv:
            descriptor:
              separator: ";"
              generateHeaderColumns: true
              crlf: UNIX
              fields:
                - {name: orderNr, pos: 1, columnName: "Order Nr"}
                - {name: amount, pos: 3, type: double, precision: 2}
                - {name: orderDate, pos: 2, type: date, pattern: yyyyMMdd}
                - {name: note, pos: 4}
"#;
    camel_yaml::add_routes_from_yaml(&c, yaml).unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let csv = "orderNr,clientNr,firstName,currency,amount,date\r\n10,  A1 ,Ann,,12.5,02-01-2009\r\n11,B2,\"Bob, Jr\",USD,3,31-12-2010\r\n";
    let out = t.request_body("direct:csv-in", csv).unwrap();
    assert_eq!(
        out.to_string(),
        r#"[{"orderNr":10,"clientNr":"A1","firstName":"Ann","amount":12.5,"orderDate":"2009-01-02","currency":"EUR"},{"orderNr":11,"clientNr":"B2","firstName":"Bob, Jr","amount":3.0,"orderDate":"2010-12-31","currency":"USD"}]"#
    );
    let missing = t.request_body("direct:csv-in", "h\r\n,x\r\n");
    assert!(missing.unwrap_err().to_string().contains("mandatory field"));
    let back = t.request_body("direct:csv-out", r#"[{"orderNr":1,"amount":2.5,"orderDate":"2020-03-04","note":"a;b"}]"#).unwrap();
    assert_eq!(back.to_string(), "Order Nr;orderDate;amount;note\n1;20200304;2.50;\"a;b\"\n");
}

#[test]
fn bindy_fixed_and_kvp_with_registry_descriptors() {
    use bindy::{BindyDescriptor, BindyField, FieldType};
    let c = ctx();
    let fixed = BindyDescriptor::fixed()
        .length(20)
        .field(BindyField::new("id", 1).length(5).field_type(FieldType::Int).padding_char('0'))
        .field(BindyField::new("name", 6).length(10).align("L").trim(true))
        .field(BindyField::new("qty", 16).length(5).field_type(FieldType::Int));
    bindy::bind_descriptor(c.registry(), "com.acme.Fixed", fixed);
    let kvp = BindyDescriptor::kvp()
        .pair_separator("\\u0001")
        .key_value_pair_separator("=")
        .field(BindyField::new("account", 1).tag(1))
        .field(BindyField::new("clOrdId", 2).tag(11).required(true))
        .field(BindyField::new("qty", 3).tag(38).field_type(FieldType::Int));
    bindy::bind_descriptor(c.registry(), "com.acme.Fix", kvp);
    c.add_routes(|r| {
        r.from("direct:fixed").unmarshal(DataFormatDefinition::new("bindyFixed").option("classType", "com.acme.Fixed")).to("mock:fixed").marshal(DataFormatDefinition::new("bindy").option("type", "Fixed").option("classType", "com.acme.Fixed"));
        r.from("direct:kvp").unmarshal(DataFormatDefinition::new("bindyKvp").option("classType", "com.acme.Fix")).to("mock:kvp").marshal(DataFormatDefinition::new("bindy-kvp").option("classType", "com.acme.Fix"));
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let out = t.request_body("direct:fixed", "00042Widget        7\r\n00007Gadget       12\r\n").unwrap();
    let m = c.mock_endpoint("mock:fixed").unwrap().received_exchanges();
    assert_eq!(m[0].body().to_string(), r#"[{"id":42,"name":"Widget","qty":7},{"id":7,"name":"Gadget","qty":12}]"#);
    assert_eq!(out.to_string(), "00042Widget        7\r\n00007Gadget       12\r\n");
    assert!(t.request_body("direct:fixed", "too short\r\n").is_err());

    let fix = "1=BE.CHM.001\u{1}11=CHM0001-01\u{1}38=200\u{1}";
    let out = t.request_body("direct:kvp", fix).unwrap();
    assert_eq!(c.mock_endpoint("mock:kvp").unwrap().received_exchanges()[0].body().to_string(), r#"{"account":"BE.CHM.001","clOrdId":"CHM0001-01","qty":200}"#);
    assert_eq!(out.to_string(), format!("{}\r\n", fix));
    assert!(t.request_body("direct:kvp", "1=x\u{1}").unwrap_err().to_string().contains("mandatory key"));
}

// ----------------------------------------------------------------------------- YAML

#[test]
fn yaml_dataformat_routes() {
    let c = ctx();
    c.add_routes(|r| {
        r.from("direct:yaml").unmarshal_json().marshal("yaml");
        r.from("direct:unyaml").unmarshal(DataFormatDefinition::new("snakeYaml")).marshal_json();
    })
    .unwrap();
    camel_yaml::add_routes_from_yaml(
        &c,
        r#"
- from:
    uri: direct:pretty
    steps:
      - unmarshal: {json: {}}
      - marshal:
          yaml:
            prettyFlow: true
- from:
    uri: direct:unsafe
    steps:
      - unmarshal:
          yaml: {}
- from:
    uri: direct:anytype
    steps:
      - unmarshal:
          yaml:
            allowAnyType: true
      - marshal: {json: {}}
"#,
    )
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    assert_eq!(t.request_body("direct:yaml", r#"{"name":"Camel"}"#).unwrap().to_string(), "{name: Camel}\n");
    assert_eq!(t.request_body("direct:yaml", r#"{"a":{"b":[1,2]},"c":"d"}"#).unwrap().to_string(), "a:\n  b: [1, 2]\nc: d\n");
    assert_eq!(t.request_body("direct:pretty", r#"{"name":"Camel"}"#).unwrap().to_string(), "{\n  name: Camel\n}\n");
    assert_eq!(t.request_body("direct:unyaml", "a: 1\nb: [x, y]\nc: {d: true}\n").unwrap().to_string(), r#"{"a":1,"b":["x","y"],"c":{"d":true}}"#);
    let e = t.request_body("direct:unsafe", "!!javax.script.ScriptEngineManager [!!java.net.URLClassLoader [[!!java.net.URL [\"http://x\"]]]]").unwrap_err();
    assert!(e.to_string().contains("Global tag is not allowed"), "{}", e);
    assert_eq!(t.request_body("direct:anytype", "!!com.acme.Pojo {name: x}").unwrap().to_string(), r#"{"name":"x"}"#);
}

// ----------------------------------------------------------------------------- Tar / Zip

#[test]
fn tar_round_trip_and_iterator() {
    let c = ctx();
    c.add_routes(|r| {
        r.from("direct:tar").marshal("tarFile").to("mock:tar").unmarshal("tarFile");
        r.from("direct:untar-it").unmarshal(DataFormatDefinition::new("tarFile").option("usingIterator", true)).split(simple("${body}")).to("mock:entries");
        r.from("direct:zip").marshal(DataFormatDefinition::new("zipFile").option("preservePathElements", true)).unmarshal(DataFormatDefinition::new("zipFile").option("usingIterator", true));
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let ex = t.send_with("direct:tar", ExchangePattern::InOut, |e| {
        e.set_body("tar me");
        e.set_header("CamelFileName", "dir/hello.txt");
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.body().to_bytes(), b"tar me");
    assert_eq!(ex.header("CamelFileName").unwrap().to_string(), "hello.txt");
    let tar_ex = &c.mock_endpoint("mock:tar").unwrap().received_exchanges()[0];
    assert_eq!(tar_ex.header("CamelFileName").unwrap().to_string(), "dir/hello.txt.tar");

    let archive = tar::tar_create(&[tar::TarEntry::file("a.txt", b"A".to_vec()), tar::TarEntry::dir("d"), tar::TarEntry::file("d/b.txt", b"BB".to_vec())]);
    t.send_body("direct:untar-it", archive).unwrap();
    let entries = c.mock_endpoint("mock:entries").unwrap().received_exchanges();
    assert_eq!(entries.iter().map(|e| e.body().to_bytes()).collect::<Vec<_>>(), vec![b"A".to_vec(), b"BB".to_vec()]);

    // multi-entry zip with paths
    let mut files = camel_core::value::ObjectMap::new();
    files.insert("x/1.txt".into(), Value::from("one"));
    files.insert("x/2.txt".into(), Value::from("two"));
    let ex = t.send_with("direct:zip", ExchangePattern::InOut, |e| e.set_body(Value::Object(files)));
    assert_eq!(ex.body(), &Value::Array(vec![Value::Bytes(b"one".to_vec()), Value::Bytes(b"two".to_vec())]));
    assert_eq!(ex.header("CamelZipFileEntryNames").unwrap().to_json(), r#"["x/1.txt","x/2.txt"]"#);
}

#[test]
fn tar_interop_with_system_tar() {
    let version = std::process::Command::new("tar").arg("--version").output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let Some(version) = version else {
        eprintln!("tar not installed: skipping");
        return;
    };
    // bsdtar (macOS, Windows) writes pax and ustar but not the gnu format
    let bsdtar = version.contains("bsdtar");
    let d = tmpdir("tar");
    let src = d.join("src");
    let long_dir = "l".repeat(120);
    std::fs::create_dir_all(src.join("sub").join(&long_dir)).unwrap();
    std::fs::create_dir_all(src.join("empty")).unwrap();
    std::fs::write(src.join("a.txt"), "alpha").unwrap();
    std::fs::write(src.join("sub").join("b.bin"), vec![0u8, 1, 2, 255]).unwrap();
    std::fs::write(src.join("sub").join(&long_dir).join("deep.txt"), "deep".repeat(300)).unwrap();
    for fmt in ["gnu", "pax", "ustar"] {
        let out = d.join(format!("{}.tar", fmt));
        let st = std::process::Command::new("tar").args(["-c", &format!("--format={}", fmt), "-f"]).arg(&out).arg("-C").arg(&src).args(["a.txt", "sub", "empty"]).output().unwrap();
        if !st.status.success() {
            assert!(fmt == "ustar" || (fmt == "gnu" && bsdtar), "{}", String::from_utf8_lossy(&st.stderr)); // ustar cannot store >100+155 names on some tars
            continue;
        }
        let entries = tar::tar_read(&std::fs::read(&out).unwrap()).unwrap();
        let find = |n: &str| entries.iter().find(|e| e.name.trim_end_matches('/') == n).unwrap_or_else(|| panic!("{} missing in {:?}", n, entries.iter().map(|e| &e.name).collect::<Vec<_>>()));
        assert_eq!(find("a.txt").data, b"alpha");
        assert_eq!(find("sub/b.bin").data, vec![0, 1, 2, 255]);
        assert_eq!(find(&format!("sub/{}/deep.txt", long_dir)).data, "deep".repeat(300).as_bytes());
        assert!(find("empty").is_dir);
    }
    // ours -> system tar
    let ours = tar::tar_create(&[tar::TarEntry::file("x.txt", b"from rust".to_vec()), tar::TarEntry::file(format!("{}/y.txt", "p".repeat(150)), b"long".to_vec()), tar::TarEntry::file(format!("{}.txt", "q".repeat(180)), b"pax".to_vec())]);
    let f = d.join("ours.tar");
    std::fs::write(&f, ours).unwrap();
    let list = std::process::Command::new("tar").arg("-tf").arg(&f).output().unwrap();
    let names = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(names.contains("x.txt") && names.contains(&format!("{}/y.txt", "p".repeat(150))) && names.contains(&format!("{}.txt", "q".repeat(180))), "{}", names);
    let cat = std::process::Command::new("tar").arg("-xOf").arg(&f).arg("x.txt").output().unwrap();
    assert_eq!(cat.stdout, b"from rust");
    let _ = std::fs::remove_dir_all(&d);
}

// ----------------------------------------------------------------------------- Crypto

#[test]
fn crypto_routes() {
    let c = ctx();
    c.registry().bind("aesKey", unhex("2b7e151628aed2a6abf7158809cf4f3c"));
    c.registry().bind("iv", unhex("000102030405060708090a0b0c0d0e0f"));
    c.add_routes(|r| {
        // known vector: NIST SP 800-38A F.2.1 (no padding, no HMAC)
        r.from("direct:nist").marshal(DataFormatDefinition::new("crypto").option("algorithm", "AES/CBC/NoPadding").option("keyRef", "aesKey").option("initVectorRef", "iv").option("shouldAppendHMAC", false));
        r.from("direct:enc")
            .marshal(DataFormatDefinition::new("crypto").option("algorithm", "AES/CBC/PKCS5Padding").option("keyRef", "aesKey").option("inline", true).option("macAlgorithm", "HmacSHA256"))
            .to("mock:enc")
            .unmarshal(DataFormatDefinition::new("crypto").option("algorithm", "AES/CBC/PKCS5Padding").option("keyRef", "aesKey").option("inline", true).option("macAlgorithm", "HmacSHA256"))
            .convert_body_to("String");
        r.from("direct:dec").unmarshal(DataFormatDefinition::new("crypto").option("algorithm", "AES/CBC/PKCS5Padding").option("keyRef", "aesKey").option("inline", true).option("macAlgorithm", "HmacSHA256"));
    })
    .unwrap();
    camel_yaml::add_routes_from_yaml(
        &c,
        r#"
- from:
    uri: direct:gcm
    steps:
      - marshal:
          crypto:
            algorithm: AES/GCM/NoPadding
            key: "hex:feffe9928665731c6d6a8f9467308308"
            initVector: "hex:cafebabefacedbaddecaf888"
            shouldAppendHMAC: false
      - to: mock:gcm
      - unmarshal:
          crypto:
            algorithm: AES/GCM/NoPadding
            key: "hex:feffe9928665731c6d6a8f9467308308"
            initVector: "hex:cafebabefacedbaddecaf888"
            shouldAppendHMAC: false
- from:
    uri: direct:des
    steps:
      - marshal:
          crypto:
            key: "hex:0123456789abcdef"
            inline: true
      - unmarshal:
          crypto:
            key: "hex:0123456789abcdef"
            inline: true
"#,
    )
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let out = t.request_body("direct:nist", unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51")).unwrap();
    assert_eq!(hex(&out.to_bytes()), "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
    assert_eq!(t.request_body("direct:enc", "Camel crypto").unwrap().to_string(), "Camel crypto");
    let enc = c.mock_endpoint("mock:enc").unwrap().received_exchanges()[0].body().to_bytes();
    assert_eq!(&enc[..4], &[0, 0, 0, 16]); // inline IV length
    assert_eq!(enc.len(), 4 + 16 + 16 + 32); // IV + one block + HMAC-SHA256
    let mut tampered = enc.clone();
    tampered[25] ^= 1;
    assert!(t.request_body("direct:dec", tampered).unwrap_err().to_string().contains("tampered"));
    assert_eq!(t.request_body("direct:gcm", "GCM text").unwrap().to_bytes(), b"GCM text");
    assert_eq!(t.request_body("direct:des", "DES/CBC/PKCS5Padding default").unwrap().to_bytes(), b"DES/CBC/PKCS5Padding default");
}

// ----------------------------------------------------------------------------- PGP

const RSA_PUB: &str = include_str!("fixtures/rsa_pub.asc");
const RSA_SEC: &str = include_str!("fixtures/rsa_sec.asc");

#[test]
fn pgp_decrypts_gnupg_messages() {
    let sec = pgp::parse_keyring(RSA_SEC.as_bytes()).unwrap();
    let public = pgp::parse_keyring(RSA_PUB.as_bytes()).unwrap();
    assert_eq!(public[0].primary.fingerprint_hex(), "3B02A59796F27ABD030332117E3CB2EB0A397594");
    assert!(sec[0].primary.unlock(Some("wrong")).is_err());
    for (file, text) in [(include_str!("fixtures/rsa_msg_ocb.asc"), "Hi from GnuPG (OCB)"), (include_str!("fixtures/rsa_msg_seipd.asc"), "Hi from GnuPG (SEIPD)")] {
        let d = pgp::decrypt(file.as_bytes(), &[(sec[0].clone(), Some("camel".into()))], &public).unwrap();
        assert_eq!(d.data, text.as_bytes());
        assert!(d.integrity_protected);
        assert!(d.signatures[0].valid);
    }
}

#[test]
fn pgp_routes_with_rsa_keys() {
    let c = ctx();
    c.registry().bind("pubring", RSA_PUB.to_string());
    c.registry().bind("secring", RSA_SEC.to_string());
    let alice = pgp::generate_ed25519("Alice <alice@example.com>", None).unwrap();
    c.registry().bind("alicePub", alice.public_keyring.clone());
    c.registry().bind("aliceSec", alice.secret_keyring.clone());
    c.add_routes(|r| {
        r.from("direct:pgp")
            .marshal(
                DataFormatDefinition::new("pgp")
                    .option("encryptionKeyRing", "#pubring")
                    .option("keyUserid", "rsa@camel.apache.org")
                    .option("signatureKeyRing", "#secring")
                    .option("signatureKeyUserid", "Camel RSA")
                    .option("signaturePassword", "camel")
                    .option("armored", true),
            )
            .to("mock:pgp")
            .unmarshal(
                DataFormatDefinition::new("pgp")
                    .option("encryptionKeyRing", "#secring")
                    .option("password", "camel")
                    .option("signatureKeyRing", "#pubring")
                    .option("signatureVerificationOption", "required"),
            )
            .convert_body_to("String");
        r.from("direct:unsigned").marshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#pubring").option("integrity", false).option("algorithm", "AES128").option("compressionAlgorithm", "2"));
        r.from("direct:required").unmarshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#secring").option("password", "camel").option("signatureKeyRing", "#pubring").option("signatureVerificationOption", "required"));
        r.from("direct:optional").unmarshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#secring").option("password", "camel"));
        r.from("direct:nosig").unmarshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#secring").option("password", "camel").option("signatureVerificationOption", "no_signature_allowed"));
        r.from("direct:wrongsigner").unmarshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#secring").option("password", "camel").option("signatureKeyRing", "#alicePub"));
        r.from("direct:alice").marshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#alicePub").option("signatureKeyRing", "#aliceSec")).unmarshal(DataFormatDefinition::new("pgp").option("encryptionKeyRing", "#aliceSec").option("signatureKeyRing", "#alicePub").option("signatureKeyUserid", "alice@"));
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    assert_eq!(t.request_body("direct:pgp", "signed and encrypted").unwrap().to_string(), "signed and encrypted");
    let m = c.mock_endpoint("mock:pgp").unwrap().received_exchanges();
    let armored = m[0].body().to_string();
    assert!(armored.starts_with("-----BEGIN PGP MESSAGE-----"));
    assert_eq!(m[0].header("CamelPGPDataFormatNumberOfEncryptionKeys"), Some(&Value::Int(1)));
    assert_eq!(m[0].header("CamelPGPDataFormatNumberOfSigningKeys"), Some(&Value::Int(1)));

    let unsigned = t.request_body("direct:unsigned", "plain").unwrap();
    assert!(t.request_body("direct:required", unsigned.clone()).unwrap_err().to_string().contains("does not contain any signatures"));
    assert_eq!(t.request_body("direct:optional", unsigned.clone()).unwrap().to_bytes(), b"plain");
    assert!(t.request_body("direct:nosig", armored.clone()).unwrap_err().to_string().contains("not expected"));
    assert!(t.request_body("direct:wrongsigner", armored.clone()).is_err());
    assert_eq!(t.request_body("direct:optional", armored).unwrap().to_bytes(), b"signed and encrypted");
    assert_eq!(t.request_body("direct:alice", "ed25519").unwrap().to_bytes(), b"ed25519");
}

// ----------------------------------------------------------------------------- others

#[test]
fn thrift_asn1_ical_mime_univocity_routes() {
    let c = ctx();
    let idl = "struct Work { 1: i32 num1, 2: i32 num2, 3: string comment }";
    let yaml = format!(
        r#"
- from:
    uri: direct:thrift
    steps:
      - unmarshal: {{json: {{}}}}
      - marshal:
          thrift:
            schema: "{idl}"
            instanceClass: org.example.Work
            contentTypeFormat: compact
      - to: mock:thrift
      - unmarshal:
          thrift:
            schema: "{idl}"
            contentTypeFormat: compact
      - marshal: {{json: {{}}}}
- from:
    uri: direct:asn1
    steps:
      - unmarshal:
          asn1:
            usingIterator: true
      - split:
          simple: "${{body}}"
          steps:
            - unmarshal: asn1
            - to: mock:asn1
- from:
    uri: direct:ical
    steps:
      - unmarshal: iCal
      - setHeader:
          name: summary
          simple: "${{body[components][0][properties][SUMMARY]}}"
      - marshal: iCal
- from:
    uri: direct:mime
    steps:
      - marshal:
          mimeMultipart:
            headersInline: true
            includeHeaders: "X-.*"
      - to: mock:mime
      - unmarshal:
          mimeMultipart:
            headersInline: true
- from:
    uri: direct:tsv
    steps:
      - unmarshal:
          univocityTsv:
            headerExtractionEnabled: true
            asMap: true
      - marshal: {{json: {{}}}}
- from:
    uri: direct:ucsv
    steps:
      - unmarshal:
          univocityCsv:
            delimiter: ";"
      - marshal: {{json: {{}}}}
- from:
    uri: direct:ufixed
    steps:
      - unmarshal:
          univocityFixed:
            fieldLengths: "3,5"
      - marshal: {{json: {{}}}}
"#
    );
    camel_yaml::add_routes_from_yaml(&c, &yaml).unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();

    let out = t.request_body("direct:thrift", r#"{"num1":1,"num2":2,"comment":"c"}"#).unwrap();
    assert_eq!(out.to_string(), r#"{"num1":1,"num2":2,"comment":"c"}"#);
    assert_eq!(hex(&c.mock_endpoint("mock:thrift").unwrap().received_exchanges()[0].body().to_bytes()), "1502150418016300");

    let rec1 = camel_dataformats::asn1::der_encode(&json::parse(r#"[1,"a"]"#).unwrap()).unwrap();
    let rec2 = camel_dataformats::asn1::der_encode(&json::parse(r#"[{"oid":"2.5.4.3"},true]"#).unwrap()).unwrap();
    t.send_body("direct:asn1", [rec1, rec2].concat()).unwrap();
    let got: Vec<String> = c.mock_endpoint("mock:asn1").unwrap().received_exchanges().iter().map(|e| e.body().to_json()).collect();
    assert_eq!(got, vec![r#"[1,"a"]"#.to_string(), r#"[{"oid":"2.5.4.3"},true]"#.to_string()]);

    let cal = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//x//y//EN\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Ride\\, camels\r\nDTSTART:20241031T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let ex = t.send_with("direct:ical", ExchangePattern::InOut, |e| e.set_body(cal));
    assert_eq!(ex.header("summary").unwrap().to_string(), "Ride, camels");
    assert_eq!(ex.body().to_string(), cal);

    let mut atts = camel_core::value::ObjectMap::new();
    atts.insert("data.bin".into(), Value::Bytes(vec![0, 1, 2, 3]));
    atts.insert("note.txt".into(), Value::from("hello"));
    let ex = t.send_with("direct:mime", ExchangePattern::InOut, |e| {
        e.set_body("main body");
        e.set_header("X-Custom", "yes");
        e.set_header("Other", "no");
        e.set_property("CamelAttachments", Value::Object(atts));
    });
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let wire = c.mock_endpoint("mock:mime").unwrap().received_exchanges()[0].body().to_string();
    assert!(wire.starts_with("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary="), "{}", wire);
    assert!(wire.contains("X-Custom: yes") && !wire.contains("Other: no"), "{}", wire);
    assert!(wire.contains("AAECAw=="), "{}", wire);
    assert_eq!(ex.body().to_string(), "main body");
    let back = ex.property("CamelAttachments").unwrap();
    assert_eq!(back.get("data.bin"), Some(&Value::Bytes(vec![0, 1, 2, 3])));
    assert_eq!(back.get("note.txt").unwrap().to_bytes(), b"hello");

    assert_eq!(t.request_body("direct:tsv", "a\tb\nx\\ty\t2\n").unwrap().to_string(), r#"[{"a":"x\ty","b":"2"}]"#);
    assert_eq!(t.request_body("direct:ucsv", "1;\"two;2\"\n").unwrap().to_string(), r#"[["1","two;2"]]"#);
    assert_eq!(t.request_body("direct:ufixed", "ab cdef\n12345678\n").unwrap().to_string(), r#"[["ab","cdef"],["123","45678"]]"#);
}

#[test]
fn unknown_options_and_names() {
    let c = ctx();
    assert!(DataFormatDefinition::new("avro").resolve(&c).is_ok()); // schema can come from instanceClassName at runtime
    assert!(DataFormatDefinition::new("crypto").option("algorithm", "Blowfish").resolve(&c).is_err());
    assert!(DataFormatDefinition::new("thrift").resolve(&c).is_err());
    assert!(DataFormatDefinition::new("bindyCsv").resolve(&c).is_err());
    // core formats still resolve
    assert!(DataFormatDefinition::new("json").resolve(&c).is_ok());
    assert!(camel_dataformats::create("pgp", &Default::default()).is_some());
    assert!(camel_dataformats::create("nope", &Default::default()).is_none());
}

// ----------------------------------------------------------------------------- Fory

#[test]
fn fory_routes_and_struct_types() {
    let c = ctx();
    c.add_routes(|b| {
        b.from("direct:fory").unmarshal_json().marshal("fory").to("mock:fory").unmarshal("fory").marshal_json();
        b.from("direct:foryStruct")
            .to("dataformat:fory:marshal?structTypes=RAW(demo.Order=id,lines)&refTracking=true")
            .to("dataformat:fory:unmarshal?structTypes=RAW(demo.Order=id,lines)");
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let json = r#"{"name":"Ann","n":-5,"f":2.5,"ok":true,"none":null,"list":[1,2,3],"nested":{"k":"v€"}}"#;
    assert_eq!(t.request_body("direct:fory", json).unwrap().to_string(), json);
    let wire = c.mock_endpoint("mock:fory").unwrap().received_exchanges()[0].body().to_bytes();
    // current xlang spec header: xlang flag, root not-null flag, MAP (24)
    assert_eq!(&wire[..3], &[0x01, 0xff, 24]);
    let order = Value::object([("@type", Value::from("demo.Order")), ("id", Value::Int(7)), ("lines", Value::from(vec!["a", "b"]))]);
    assert_eq!(t.request_body("direct:foryStruct", order.clone()).unwrap(), order);
    // the Fory 0.10/0.11 layout on request (bitmap + language byte)
    let legacy = DataFormatDefinition::new("fory").option("xlangSpec", "LEGACY").resolve(&c).unwrap();
    let mut ex = Exchange::new(None);
    ex.message.body = Value::from("x");
    legacy.marshal(&mut ex).unwrap();
    assert_eq!(&ex.body().to_bytes()[..4], &[0x06, 0x06, 0xff, 0x0c]);
    legacy.unmarshal(&mut ex).unwrap();
    assert_eq!(ex.body(), &Value::from("x"));
    assert!(DataFormatDefinition::new("fory").option("xlangSpec", "0.9").resolve(&c).is_err());
    assert!(DataFormatDefinition::new("fory").option("requireClassRegistration", "true").option("threadSafe", "false").option("allowAutoWiredFory", "false").resolve(&c).is_ok());
    // YAML DSL
    let y = ctx();
    camel_yaml::add_routes_from_yaml(&y, "- from:\n    uri: direct:y\n    steps:\n      - marshal:\n          fory: {}\n      - unmarshal:\n          fory: {}\n").unwrap();
    y.start().unwrap();
    assert_eq!(y.create_producer_template().request_body("direct:y", "hello").unwrap(), Value::from("hello"));
}

// ----------------------------------------------------------------------------- Barcode

#[test]
fn barcode_routes_and_third_party_pngs() {
    let c = ctx();
    c.add_routes(|b| {
        b.from("direct:qr").marshal("barcode").to("mock:png").unmarshal("barcode");
        b.from("direct:ean")
            .marshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "EAN_13").option("width", 300).option("height", 120))
            .unmarshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "EAN_13"));
        b.from("direct:dm").to("dataformat:barcode:marshal?barcodeFormat=DATA_MATRIX&width=200&height=200").to("dataformat:barcode:unmarshal");
        b.from("direct:qrH").to("dataformat:barcode:marshal?errorCorrectionLevel=H").to("dataformat:barcode:unmarshal");
        b.from("direct:pdf").to("dataformat:barcode:marshal?barcodeFormat=PDF_417&errorCorrectionLevel=4&imageType=JPG").to("dataformat:barcode:unmarshal");
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let ex = t.request("direct:qr", |e| e.set_body("Hello from rust-camel")).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.body().to_string(), "Hello from rust-camel");
    assert_eq!(ex.header("BarcodeFormat").unwrap().to_string(), "QR_CODE");
    let png = c.mock_endpoint("mock:png").unwrap().received_exchanges()[0].body().to_bytes();
    assert_eq!(&png[1..4], b"PNG");
    let ex = t.request("direct:ean", |e| e.set_body("590123412345")).unwrap();
    assert_eq!((ex.body().to_string(), ex.header("BarcodeFormat").unwrap().to_string()), ("5901234123457".to_string(), "EAN_13".to_string()));
    let ex = t.request("direct:dm", |e| e.set_body("ECC200 data")).unwrap();
    assert_eq!((ex.body().to_string(), ex.header("BarcodeFormat").unwrap().to_string()), ("ECC200 data".to_string(), "DATA_MATRIX".to_string()));
    let ex = t.request("direct:qrH", |e| e.set_body("level H")).unwrap();
    assert_eq!(ex.header("ERROR_CORRECTION_LEVEL").unwrap().to_string(), "H");
    let ex = t.request("direct:pdf", |e| e.set_body("PDF417 shipping label 0123456789012345")).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!((ex.body().to_string(), ex.header("BarcodeFormat").unwrap().to_string()), ("PDF417 shipping label 0123456789012345".to_string(), "PDF_417".to_string()));

    // PNGs re-encoded by Pillow (RGB interlaced, palette, RGBA) from our output
    let fx = |n: &str| std::fs::read(format!("{}/tests/fixtures/barcode/{}", env!("CARGO_MANIFEST_DIR"), n)).unwrap();
    let df = DataFormatDefinition::new("barcode").resolve(&c).unwrap();
    for (f, text) in [("qr_rgb_interlaced.png", "Hello 123"), ("qr_pal.png", "Hello 123"), ("dm_rgba.png", "Hello 123")] {
        let mut ex = Exchange::new(None);
        ex.message.body = Value::Bytes(fx(f));
        df.unmarshal(&mut ex).unwrap();
        assert_eq!(ex.body().to_string(), text, "{}", f);
    }
    assert!(DataFormatDefinition::new("barcode").option("imageType", "JPG").resolve(&c).is_ok());
    assert!(DataFormatDefinition::new("barcode").option("imageType", "BMP").resolve(&c).is_err());
    // MaxiCode is decode-only (ZXing has no MaxiCode writer): marshalling fails
    let maxi = DataFormatDefinition::new("barcode").option("barcodeFormat", "MAXICODE").resolve(&c).unwrap();
    let mut ex = Exchange::new(None);
    ex.message.body = Value::String("x".into());
    assert!(maxi.marshal(&mut ex).is_err());
    assert!(DataFormatDefinition::new("barcode").option("barcodeFormat", "BOGUS").resolve(&c).is_err());
    // multiple=true: every symbol of the image as a list of maps
    {
        use camel_dataformats::barcode::{encode, png, qr::Ecc, write_image, Format, ImageType};
        let a = encode("first", Format::QrCode, Ecc::M, 200, 200).unwrap();
        let b = encode("SECOND-39", Format::Code39, Ecc::M, 320, 100).unwrap();
        let mut canvas = png::Gray { width: 600, height: 240, pixels: vec![255; 600 * 240] };
        for (img, x0, y0) in [(&a, 10usize, 20usize), (&b, 260, 70)] {
            for y in 0..img.height {
                for x in 0..img.width {
                    canvas.pixels[(y0 + y) * 600 + x0 + x] = img.pixels[y * img.width + x];
                }
            }
        }
        let multi = DataFormatDefinition::new("barcode").option("multiple", "true").resolve(&c).unwrap();
        let mut ex = Exchange::new(None);
        ex.message.body = Value::Bytes(write_image(&canvas, ImageType::Png));
        multi.unmarshal(&mut ex).unwrap();
        let mut got: Vec<(String, String)> = ex
            .body()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v.get("text").unwrap().as_str().unwrap().to_string(), v.get("BarcodeFormat").unwrap().as_str().unwrap().to_string()))
            .collect();
        got.sort();
        assert_eq!(got, vec![("SECOND-39".to_string(), "CODE_39".to_string()), ("first".to_string(), "QR_CODE".to_string())]);
        assert_eq!(ex.header("BarcodeCount").unwrap().as_i64(), Some(2));
    }
    // the further linear symbologies through a route, with format headers
    for (fmt, text, want) in [("CODABAR", "A40156B", "40156"), ("CODE_93", "Code 93!", "Code 93!"), ("EAN_8", "9638507", "96385074"), ("ITF", "30712345000010", "30712345000010"), ("UPC_E", "0123456", "01234565")] {
        let m = DataFormatDefinition::new("barcode").option("barcodeFormat", fmt).option("width", 400).option("height", 100).resolve(&c).unwrap();
        let u = DataFormatDefinition::new("barcode").resolve(&c).unwrap();
        let mut ex = Exchange::new(None);
        ex.message.body = Value::String(text.into());
        m.marshal(&mut ex).unwrap();
        u.unmarshal(&mut ex).unwrap();
        assert_eq!((ex.body().to_string(), ex.header("BarcodeFormat").unwrap().to_string()), (want.to_string(), fmt.to_string()));
    }
    assert!(DataFormatDefinition::new("barcode").option("barcodeFormat", "PDF_417").option("errorCorrectionLevel", "9").resolve(&c).is_err());
}

#[test]
fn asn1_typed_module_routes() {
    let module = "M DEFINITIONS AUTOMATIC TAGS ::= BEGIN Rec ::= SEQUENCE { id INTEGER, name UTF8String OPTIONAL, ok BOOLEAN DEFAULT TRUE } END";
    let c = ctx();
    c.add_routes(|r| {
        r.from("direct:asn1-typed")
            .marshal(DataFormatDefinition::new("asn1").option("schema", module).option("clazzName", "com.example.Rec"))
            .to("mock:asn1-typed")
            .unmarshal(DataFormatDefinition::new("asn1").option("schema", module).option("clazzName", "Rec"))
            .marshal_json();
        r.from("direct:asn1-iter").unmarshal(DataFormatDefinition::new("asn1").option("schema", module).option("clazzName", "Rec").option("usingIterator", "true")).marshal_json();
    })
    .unwrap();
    c.start().unwrap();
    let t = c.create_producer_template();
    let out = t.request_body("direct:asn1-typed", r#"{"id":5,"name":"x"}"#).unwrap();
    assert_eq!(out.to_string(), r#"{"id":5,"name":"x","ok":true}"#);
    assert_eq!(hex(&c.mock_endpoint("mock:asn1-typed").unwrap().received_exchanges()[0].body().to_bytes()), "3006800105810178");
    let recs = [unhex("3003800101"), unhex("3006800102820100")].concat();
    let out = t.request_body("direct:asn1-iter", recs).unwrap();
    assert_eq!(out.to_string(), r#"[{"id":1,"ok":true},{"id":2,"ok":false}]"#);
    assert!(DataFormatDefinition::new("asn1").option("schema", module).resolve(&c).is_err());
}
