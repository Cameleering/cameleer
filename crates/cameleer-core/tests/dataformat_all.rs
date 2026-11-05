//! Data format tests: round trips through routes and interoperability vectors.

use cameleer_core::dataformat::compress::{gzip_compress, gzip_decompress, inflate, zip_create, zip_read, zlib_decompress, ZipEntry};
use cameleer_core::dataformat::{bind_data_format, create_data_format, DataFormat, DataFormatDefinition};
use cameleer_core::prelude::*;

/// `printf 'Hello, Apache Camel! ' | gzip -9 -n` (fixed Huffman block).
const GZIP_SHORT: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x70, 0x2c, 0x48, 0x4c, 0xce, 0x48,
    0x55, 0x70, 0x4e, 0xcc, 0x4d, 0xcd, 0x51, 0x54, 0x00, 0x00, 0x22, 0x30, 0x4d, 0x75, 0x15, 0x00, 0x00, 0x00,
];

/// 30 lines "Line N: the quick brown fox ..." compressed with `gzip -9 -n` (dynamic Huffman block).
const GZIP_LINES: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xb5, 0xd5, 0xcb, 0x0d, 0xc2, 0x40, 0x0c, 0x45, 0xd1, 0x3d, 0x55, 0xb8, 0x82,
    0x08, 0x3b, 0xfc, 0xb7, 0x6c, 0x69, 0x22, 0x10, 0x03, 0x01, 0x32, 0x13, 0x32, 0x13, 0x02, 0x54, 0xcf, 0xa7, 0x87, 0xbb, 0xb3, 0xf4, 0xa4,
    0xbb, 0x3b, 0xf2, 0xae, 0x09, 0x2e, 0xba, 0x91, 0x7c, 0x76, 0xb9, 0x0f, 0xcd, 0xe1, 0x2a, 0xfb, 0x3e, 0x8e, 0x41, 0x8e, 0xf1, 0x29, 0x97,
    0xa1, 0xed, 0x92, 0xc4, 0x87, 0xf7, 0xff, 0xf9, 0x56, 0xbd, 0x5f, 0x52, 0xc7, 0x53, 0x21, 0xdb, 0xaa, 0xf5, 0x9b, 0xf4, 0x71, 0xc8, 0x9e,
    0xa4, 0xf5, 0x94, 0xaa, 0xd3, 0xf7, 0xd8, 0x7b, 0x1e, 0xdd, 0x83, 0x78, 0xa8, 0xbb, 0xd8, 0x84, 0x9c, 0x8a, 0xc9, 0xee, 0x57, 0x37, 0xb4,
    0x5e, 0xa2, 0xf5, 0x19, 0x5a, 0x9f, 0xa3, 0xf5, 0x05, 0x5a, 0x5f, 0xa2, 0xf5, 0x15, 0x5a, 0x5f, 0xa3, 0x75, 0x9d, 0xb2, 0x79, 0x56, 0xab,
    0xb2, 0x5c, 0x95, 0xf5, 0xaa, 0x2c, 0x58, 0x65, 0xc5, 0x2a, 0x4b, 0x56, 0x59, 0xb3, 0xca, 0xa2, 0x55, 0x56, 0xad, 0xb1, 0x6a, 0x0d, 0xfe,
    0xb1, 0xac, 0x5a, 0x63, 0xd5, 0x1a, 0xab, 0xd6, 0x58, 0xb5, 0xc6, 0xaa, 0x35, 0x56, 0xad, 0xb1, 0x6a, 0x8d, 0x55, 0x5b, 0x62, 0x6a, 0x3f,
    0x5c, 0xd0, 0xc8, 0xfc, 0x19, 0x0b, 0x00, 0x00,
];

fn lines_text() -> String {
    (1..=30).map(|i| format!("Line {}: the quick brown fox jumps over the lazy dog. Camel routes messages between endpoints.\n", i)).collect()
}

fn run(df: DataFormatDefinition, marshal: bool, body: impl Into<Value>) -> Result<Exchange> {
    let ctx = CamelContext::new();
    let d = df.resolve(&ctx)?;
    let mut ex = ctx.create_exchange();
    ex.set_body(body);
    if marshal {
        d.marshal(&mut ex)?;
    } else {
        d.unmarshal(&mut ex)?;
    }
    Ok(ex)
}

#[test]
fn gzip_interop_with_standard_gzip() {
    assert_eq!(gzip_decompress(GZIP_SHORT).unwrap(), b"Hello, Apache Camel! ");
    let text = lines_text();
    assert_eq!(text.len(), 2841);
    assert_eq!(gzip_decompress(GZIP_LINES).unwrap(), text.as_bytes());
    // concatenated members decode as one stream
    let mut both = GZIP_SHORT.to_vec();
    both.extend_from_slice(GZIP_LINES);
    let out = gzip_decompress(&both).unwrap();
    assert_eq!(out.len(), 21 + 2841);
    // via the data format
    let ex = run(DataFormatDefinition::gzip(), false, GZIP_LINES.to_vec()).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), text);
    // our output has a valid header/trailer that our decoder (and gzip) accept
    let ours = gzip_compress(text.as_bytes(), 9);
    assert_eq!(&ours[..3], &[0x1f, 0x8b, 0x08]);
    assert!(ours.len() < 600, "compressed size {}", ours.len());
    assert_eq!(gzip_decompress(&ours).unwrap(), text.as_bytes());
    // corrupted data is detected
    let mut bad = GZIP_LINES.to_vec();
    bad[40] ^= 0x55;
    assert!(gzip_decompress(&bad).is_err());
    assert!(gzip_decompress(&GZIP_LINES[..60]).is_err());
}

#[test]
fn system_gzip_decodes_our_output() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let text = lines_text().repeat(3);
    for level in [0, 1, 9] {
        let ours = gzip_compress(text.as_bytes(), level);
        let child = Command::new("gzip").arg("-dc").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(_) => return, // gzip not installed: skip
        };
        child.stdin.take().unwrap().write_all(&ours).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "gzip rejected level {} output: {}", level, String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout, text.as_bytes());
    }
}

#[test]
fn deflate_zlib_zip_roundtrips() {
    let data: Vec<u8> = (0..20000u32).map(|i| ((i * 7) % 251) as u8).chain(b"tail tail tail tail".iter().copied()).collect();
    for df in [DataFormatDefinition::gzip(), DataFormatDefinition::zip_deflater(), DataFormatDefinition::gzip().compression_level(0)] {
        let ex = run(df.clone(), true, data.clone()).unwrap();
        let compressed = ex.body().to_bytes();
        let ex2 = run(df, false, compressed).unwrap();
        assert_eq!(ex2.body().to_bytes(), data);
    }
    // zlib header from java.util.zip.Deflater default level
    let ex = run(DataFormatDefinition::zip_deflater(), true, "abc").unwrap();
    let z = ex.body().to_bytes();
    assert_eq!(z[0], 0x78);
    assert_eq!(((z[0] as u16) << 8 | z[1] as u16) % 31, 0);
    assert_eq!(zlib_decompress(&z).unwrap(), b"abc");
    // python zlib.compress(b"hello hello hello hello", 9)
    let py = [0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08, 0xb1];
    assert_eq!(zlib_decompress(&py).unwrap(), b"hello hello hello hello");
    // raw stored block
    assert_eq!(inflate(&[0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c']).unwrap(), b"abc");
    // zip file format
    let ctx = CamelContext::new();
    let d = DataFormatDefinition::zip_file().resolve(&ctx).unwrap();
    let mut ex = ctx.create_exchange();
    ex.set_header(props::FILE_NAME, "dir/report.txt");
    ex.set_body("zip me ".repeat(50));
    d.marshal(&mut ex).unwrap();
    assert_eq!(ex.header(props::FILE_NAME), Some(&Value::from("dir/report.txt.zip")));
    let entries = zip_read(&ex.body().to_bytes()).unwrap();
    assert_eq!(entries[0].name, "report.txt");
    d.unmarshal(&mut ex).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "zip me ".repeat(50));
    assert_eq!(ex.header(props::FILE_NAME), Some(&Value::from("report.txt")));
    let multi = zip_create(&[ZipEntry { name: "a".into(), data: b"1".to_vec() }, ZipEntry { name: "b".into(), data: b"2".to_vec() }], 0);
    let ex = run(DataFormatDefinition::zip_file().option("usingIterator", true), false, multi).unwrap();
    assert_eq!(ex.body(), &Value::Array(vec![Value::Bytes(b"1".to_vec()), Value::Bytes(b"2".to_vec())]));
}

#[test]
fn json_format() {
    let obj = cameleer_core::json::parse(r#"{"a":1,"b":[true,null,"x"]}"#).unwrap();
    let ex = run(DataFormatDefinition::json(), true, obj.clone()).unwrap();
    assert_eq!(ex.body(), &Value::from(r#"{"a":1,"b":[true,null,"x"]}"#));
    assert_eq!(ex.header("Content-Type"), Some(&Value::from("application/json")));
    let ex = run(DataFormatDefinition::json().pretty_print(true), true, obj.clone()).unwrap();
    assert!(ex.body().to_string().contains("\n  \"a\": 1"));
    let ex = run(DataFormatDefinition::json(), false, r#"{"a":1,"b":[true,null,"x"]}"#).unwrap();
    assert_eq!(ex.body(), &obj);
    let ex = run(DataFormatDefinition::json(), false, Value::Bytes(br#"[1,2]"#.to_vec())).unwrap();
    assert_eq!(ex.body(), &Value::from(vec![Value::Int(1), Value::Int(2)]));
    let ex = run(DataFormatDefinition::json(), true, "text").unwrap();
    assert_eq!(ex.body(), &Value::from("\"text\""));
    let ex = run(DataFormatDefinition::json().option("contentTypeHeader", false), true, 1).unwrap();
    assert!(ex.header("Content-Type").is_none());
    let err = run(DataFormatDefinition::json(), false, "{broken").unwrap_err();
    assert!(err.is_a(kinds::PARSE));
    assert!(run(DataFormatDefinition::json().option("prettyPrint", "maybe"), true, 1).is_err());
}

#[test]
fn csv_format() {
    // unmarshal as lists
    let ex = run(DataFormatDefinition::csv(), false, "a,b,c\r\n1,\"2,5\",3\r\n").unwrap();
    assert_eq!(ex.body(), &Value::from(vec![Value::from(vec!["a", "b", "c"]), Value::from(vec!["1", "2,5", "3"])]));
    // maps with header line
    let ex = run(DataFormatDefinition::csv().use_maps(true), false, "id,name\n1,Ann\n2,\"Bob \"\"B\"\"\"\n").unwrap();
    let rows = ex.body().as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].get("name"), Some(&Value::from("Bob \"B\"")));
    assert_eq!(rows[0].get("id"), Some(&Value::from("1")));
    // configured header + delimiter
    let ex = run(DataFormatDefinition::csv().delimiter(';').header(&["x", "y"]).use_maps(true), false, "1;2\n3;4").unwrap();
    assert_eq!(ex.body().as_array().unwrap()[1].get("y"), Some(&Value::from("4")));
    // skip header record
    let ex = run(DataFormatDefinition::csv().skip_header_record(true), false, "h1,h2\nv1,v2\n").unwrap();
    assert_eq!(ex.body(), &Value::from(vec![Value::from(vec!["v1", "v2"])]));
    // tab delimiter by name
    let ex = run(DataFormatDefinition::csv().option("delimiter", "\\t"), false, "a\tb").unwrap();
    assert_eq!(ex.body(), &Value::from(vec![Value::from(vec!["a", "b"])]));
    // marshal lists
    let rows = Value::from(vec![Value::from(vec!["a", "b c", "d,e"]), Value::from(vec![Value::Int(1), Value::Float(2.5), Value::Null])]);
    let ex = run(DataFormatDefinition::csv(), true, rows.clone()).unwrap();
    assert_eq!(ex.body(), &Value::from("a,b c,\"d,e\"\r\n1,2.5,\r\n"));
    // marshal maps with header, then round-trip back
    let maps = cameleer_core::json::parse(r#"[{"id":"1","name":"Ann"},{"id":"2","name":"Bob, Jr"}]"#).unwrap();
    let ex = run(DataFormatDefinition::csv().use_maps(true), true, maps.clone()).unwrap();
    assert_eq!(ex.body(), &Value::from("id,name\r\n1,Ann\r\n2,\"Bob, Jr\"\r\n"));
    let back = run(DataFormatDefinition::csv().use_maps(true), false, ex.body().clone()).unwrap();
    assert_eq!(back.body(), &maps);
    // marshal maps with explicit header order
    let ex = run(DataFormatDefinition::csv().header(&["name", "id"]).delimiter(';'), true, maps).unwrap();
    assert_eq!(ex.body(), &Value::from("name;id\r\nAnn;1\r\nBob, Jr;2\r\n"));
    // quote mode ALL
    let ex = run(DataFormatDefinition::csv().option("quoteMode", "ALL"), true, Value::from(vec!["a", "b"])).unwrap();
    assert_eq!(ex.body(), &Value::from("\"a\",\"b\"\r\n"));
    // single object
    let ex = run(DataFormatDefinition::csv(), true, Value::object([("k", "v")])).unwrap();
    assert_eq!(ex.body(), &Value::from("v\r\n"));
    // malformed
    assert!(run(DataFormatDefinition::csv(), false, "\"open").is_err());
}

#[test]
fn base64_string_hex_xml_urlencoded() {
    let ex = run(DataFormatDefinition::base64(), true, "Hello Camel").unwrap();
    assert_eq!(ex.body(), &Value::from("SGVsbG8gQ2FtZWw="));
    let ex = run(DataFormatDefinition::base64(), false, "SGVsbG8gQ2FtZWw=").unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "Hello Camel");
    let long = vec![0xAAu8; 200];
    let ex = run(DataFormatDefinition::base64(), true, long.clone()).unwrap();
    let text = ex.body().to_string();
    assert!(text.lines().all(|l| l.trim_end().len() <= 76));
    assert_eq!(text.lines().next().unwrap().len(), 76);
    assert_eq!(run(DataFormatDefinition::base64(), false, text).unwrap().body().to_bytes(), long);
    let ex = run(DataFormatDefinition::base64().line_length(0), true, long.clone()).unwrap();
    assert!(!ex.body().to_string().contains('\n'));
    let ex = run(DataFormatDefinition::base64().url_safe(true), true, vec![0xfbu8, 0xff, 0xbf]).unwrap();
    assert_eq!(ex.body(), &Value::from("-_-_"));
    assert!(run(DataFormatDefinition::base64(), false, "**").is_err());

    let ex = run(DataFormatDefinition::string(), true, "héllo").unwrap();
    assert_eq!(ex.body(), &Value::Bytes("héllo".as_bytes().to_vec()));
    let ex = run(DataFormatDefinition::string(), false, Value::Bytes("héllo".as_bytes().to_vec())).unwrap();
    assert_eq!(ex.body(), &Value::from("héllo"));
    let ex = run(DataFormatDefinition::string().charset("ISO-8859-1"), true, "é").unwrap();
    assert_eq!(ex.body(), &Value::Bytes(vec![0xe9]));
    let ex = run(DataFormatDefinition::string().charset("ISO-8859-1"), false, Value::Bytes(vec![0xe9])).unwrap();
    assert_eq!(ex.body(), &Value::from("é"));

    let ex = run(DataFormatDefinition::hex(), true, Value::Bytes(vec![0, 15, 255])).unwrap();
    assert_eq!(ex.body(), &Value::from("000fff"));
    let ex = run(DataFormatDefinition::hex(), false, "000FFF").unwrap();
    assert_eq!(ex.body(), &Value::Bytes(vec![0, 15, 255]));
    assert!(run(DataFormatDefinition::hex(), false, "abc").is_err());

    let v = cameleer_core::json::parse(r#"{"id":"7","item":["a","b"],"total":"3"}"#).unwrap();
    let ex = run(DataFormatDefinition::xml().root_name("order"), true, v.clone()).unwrap();
    assert_eq!(ex.body(), &Value::from("<order><id>7</id><item>a</item><item>b</item><total>3</total></order>"));
    let back = run(DataFormatDefinition::xml(), false, ex.body().clone()).unwrap();
    assert_eq!(back.body(), &v);
    assert!(run(DataFormatDefinition::xml(), false, "<a>").is_err());

    let ex = run(DataFormatDefinition::urlencoded(), true, Value::object([("q", "a b&c"), ("n", "1")])).unwrap();
    assert_eq!(ex.body(), &Value::from("q=a%20b%26c&n=1"));
    let ex = run(DataFormatDefinition::urlencoded(), false, "q=a+b%26c&n=1&n=2").unwrap();
    assert_eq!(ex.body().get("q"), Some(&Value::from("a b&c")));
    assert_eq!(ex.body().get("n"), Some(&Value::from(vec!["1", "2"])));
}

struct Reverse;
impl DataFormat for Reverse {
    fn name(&self) -> &str {
        "reverse"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let s: String = ex.body().to_string().chars().rev().collect();
        ex.set_body(s);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        self.marshal(ex)
    }
}

#[test]
fn marshal_unmarshal_in_routes() {
    let ctx = CamelContext::new();
    bind_data_format(ctx.registry(), "myDf", Reverse);
    ctx.properties().set("csv.delim", ";");
    ctx.add_routes(|b| {
        b.from("direct:json")
            .unmarshal_json()
            .set_header("name", simple("${body.name}"))
            .set_body(simple("${body}"))
            .marshal(DataFormatDefinition::json().pretty_print(false))
            .to("mock:json");
        b.from("direct:csv")
            .unmarshal(DataFormatDefinition::csv().option("delimiter", "{{csv.delim}}").use_maps(true))
            .marshal_json()
            .to("mock:csv");
        b.from("direct:zip").marshal(DataFormatDefinition::gzip()).marshal("base64").unmarshal("base64").unmarshal("gzipDeflater").to("mock:zip");
        b.from("direct:custom").marshal(DataFormatDefinition::named("myDf")).to("mock:custom");
        b.from("direct:instance").unmarshal(DataFormatDefinition::custom(Reverse)).to("mock:instance");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();

    let m = ctx.mock_endpoint("mock:json").unwrap();
    m.expected_bodies_received(vec![r#"{"name":"Ann","age":3}"#]);
    m.expected_header_received("name", "Ann");
    t.send_body("direct:json", r#"{"name": "Ann", "age": 3}"#).unwrap();
    m.assert_is_satisfied().unwrap();

    let m = ctx.mock_endpoint("mock:csv").unwrap();
    m.expected_bodies_received(vec![r#"[{"a":"1","b":"2"}]"#]);
    t.send_body("direct:csv", "a;b\n1;2\n").unwrap();
    m.assert_is_satisfied().unwrap();

    let m = ctx.mock_endpoint("mock:zip").unwrap();
    t.send_body("direct:zip", "round trip ".repeat(20)).unwrap();
    assert_eq!(m.received_exchanges()[0].body_as::<String>().unwrap(), "round trip ".repeat(20));

    let m = ctx.mock_endpoint("mock:custom").unwrap();
    m.expected_bodies_received(vec!["cba"]);
    t.send_body("direct:custom", "abc").unwrap();
    m.assert_is_satisfied().unwrap();

    let m = ctx.mock_endpoint("mock:instance").unwrap();
    m.expected_bodies_received(vec!["zyx"]);
    t.send_body("direct:instance", "xyz").unwrap();
    m.assert_is_satisfied().unwrap();

    // unmarshal failure propagates
    assert!(t.send_body("direct:json", "{nope").is_err());
    ctx.stop().unwrap();

    // unknown data format fails at route creation
    let ctx2 = CamelContext::new();
    ctx2.add_routes(|b| {
        b.from("direct:x").marshal("doesNotExist").to("mock:x");
    })
    .unwrap();
    let e = ctx2.start().unwrap_err();
    assert!(e.to_string().contains("doesNotExist"), "{}", e);
    assert!(create_data_format("csv", &Default::default()).is_ok());
}
