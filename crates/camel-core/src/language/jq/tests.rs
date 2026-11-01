use super::*;

fn j(s: &str) -> Value {
    normalize(crate::json::parse(s).unwrap_or_else(|e| panic!("bad json {}: {}", s, e)))
}

/// (program, input, expected outputs as a JSON array)
fn check(cases: &[(&str, &str, &str)]) {
    for (q, input, expected) in cases {
        let jq = Jq::compile(q).unwrap_or_else(|e| panic!("compile {}: {}", q, e));
        let got = jq.outputs(&Value::String(input.to_string())).unwrap_or_else(|e| panic!("run {}: {}", q, e));
        let exp = match j(expected) {
            Value::Array(a) => a,
            _ => unreachable!(),
        };
        assert_eq!(
            Value::Array(got.clone()).to_json(),
            Value::Array(exp.clone()).to_json(),
            "jq '{}' on {}",
            q,
            input
        );
    }
}

fn check_err(cases: &[(&str, &str, &str)]) {
    for (q, input, msg) in cases {
        let e = Jq::compile(q).and_then(|jq| jq.outputs(&Value::String(input.to_string()))).expect_err(q);
        assert!(e.message.contains(msg), "jq '{}': expected error containing {:?}, got {:?}", q, msg, e.message);
    }
}

#[test]
fn paths_and_basics() {
    check(&[
        (".", "1", "[1]"),
        (".a.b", r#"{"a":{"b":2}}"#, "[2]"),
        (".[1:3]", "[1,2,3,4]", "[[2,3]]"),
        (".[-2:]", r#""abcd""#, r#"["cd"]"#),
        (".[2]?", r#"{"a":1}"#, "[]"),
        ("[.[]?]", "1", "[[]]"),
        (".[\"a\"]", r#"{"a":5}"#, "[5]"),
        (".a.[0]", r#"{"a":[7]}"#, "[7]"),
        ("..|numbers", "[1,[2]]", "[1,2]"),
        ("[paths]", r#"{"a":[1,{"b":2}]}"#, r#"[[["a"],["a",0],["a",1],["a",1,"b"]]]"#),
        ("[leaf_paths]", r#"{"a":[1,{"b":2}]}"#, r#"[[["a",0],["a",1,"b"]]]"#),
        ("path(.a[0].b)", "null", r#"[["a",0,"b"]]"#),
        ("[path(..)]", "[[1]]", "[[[],[0],[0,0]]]"),
        ("path(.a[1:2])", "null", r#"[["a",{"start":1,"end":2}]]"#),
        ("[path(.[] | select(. > 1))]", "[1,2,3]", "[[[1],[2]]]"),
        ("getpath([\"a\",\"b\"])", "null", "[null]"),
        ("setpath([\"a\",1]; 5)", "null", r#"[{"a":[null,5]}]"#),
        ("delpaths([[\"a\"],[\"b\",0]])", r#"{"a":1,"b":[1,2],"c":3}"#, r#"[{"b":[2],"c":3}]"#),
        ("del(.[1,2])", "[1,2,3,4]", "[[1,4]]"),
        ("del(.. | select(. == null))", r#"[1,null,{"a":null,"b":2}]"#, r#"[[1,{"b":2}]]"#),
        ("to_entries", r#"{"a":1,"b":2}"#, r#"[[{"key":"a","value":1},{"key":"b","value":2}]]"#),
        ("from_entries", r#"[{"key":"a","value":1},{"k":"b","v":2},{"name":"c","value":3},{"key":1,"value":4}]"#, r#"[{"a":1,"b":2,"c":3,"1":4}]"#),
        ("with_entries(.value += 1)", r#"{"a":1,"b":2}"#, r#"[{"a":2,"b":3}]"#),
        ("pick(.a, .b.c)", r#"{"a":1,"b":{"c":2,"d":3},"e":4}"#, r#"[{"a":1,"b":{"c":2}}]"#),
        ("tostream", r#"{"a":[1,2]}"#, r#"[[["a",0],1],[["a",1],2],[["a",1]],[["a"]]]"#),
        ("fromstream(tostream)", r#"{"a":[1,{"b":2}]}"#, r#"[{"a":[1,{"b":2}]}]"#),
        ("[1|truncate_stream([[0],1],[[1,0],2],[[1,0]],[[1]])]", "null", "[[[[0],2],[[0]]]]"),
        ("$__loc__", "null", r#"[{"file":"<stdin>","line":1}]"#),
        ("{a: 1} | .a as $x | $x + 1", "null", "[2]"),
    ]);
}

#[test]
fn operators_and_constructs() {
    check(&[
        ("[(1,2) + (10,20)]", "null", "[[11,12,21,22]]"),
        ("1 / 3 * 3", "null", "[1]"),
        ("5 % -2, -5 % 2", "null", "[1,-1]"),
        ("\"abc\" * 2", "null", r#"["abcabc"]"#),
        ("\"a,b\" / \",\"", "null", r#"[["a","b"]]"#),
        ("{a:{b:1}} * {a:{c:2}}", "null", r#"[{"a":{"b":1,"c":2}}]"#),
        ("[1,2,2,3] - [2]", "null", "[[1,3]]"),
        ("[(true,false) and (true,false)]", "null", "[[true,false,false]]"),
        ("(false, null, 1) // 2", "null", "[1]"),
        ("empty // 2", "null", "[2]"),
        ("(1, error(\"x\"), 2) // 3", "null", "[1]"),
        (".a = 1", "{}", r#"[{"a":1}]"#),
        (".a |= . + 1", r#"{"a":1,"b":2}"#, r#"[{"a":2,"b":2}]"#),
        (".[] += 10", "[1,2]", "[[11,12]]"),
        (".a //= 5", r#"{"a":null}"#, r#"[{"a":5}]"#),
        (".[] |= empty", "[1,2,3]", "[[]]"),
        ("map_values(. * 2)", r#"{"a":1}"#, r#"[{"a":2}]"#),
        (".a = (1,2)", "{}", r#"[{"a":1},{"a":2}]"#),
        ("if . then 1 end", "false", "[false]"),
        ("if . == 1 then \"one\" elif . == 2 then \"two\" else \"many\" end", "2", r#"["two"]"#),
        ("try error(\"x\") catch .", "null", r#"["x"]"#),
        ("try error({a:1}) catch .a", "null", "[1]"),
        ("[.[] | try (if . == 2 then error(\"no\") else . end)]", "[1,2,3]", "[[1,3]]"),
        ("try error(null) catch .", "null", "[null]"),
        ("[.[] | (1 / .)?]", "[1,0]", "[[1]]"),
        ("reduce .[] as $x (0; . + $x)", "[1,2,3]", "[6]"),
        ("reduce .[] as [$a, $b] (0; . + $a * $b)", "[[1,2],[3,4]]", "[14]"),
        ("[foreach .[] as $x (0; . + $x)]", "[1,2,3]", "[[1,3,6]]"),
        ("[foreach .[] as $x (0; . + $x; [$x, .])]", "[1,2]", "[[[1,1],[2,3]]]"),
        ("[label $out | .[] | if . > 2 then break $out else . end]", "[1,2,3,4]", "[[1,2]]"),
        ("[limit(3; range(10))]", "null", "[[0,1,2]]"),
        ("[limit(3; repeat(1))]", "null", "[[null,1,1]]"),
        ("first(range(10; 0; -1))", "null", "[10]"),
        ("[range(0; 10; 3)]", "null", "[[0,3,6,9]]"),
        ("[range(5; 0; -2)]", "null", "[[5,3,1]]"),
        ("[first, last, nth(1)]", "[1,2,3]", "[[1,3,2]]"),
        ("[nth(2; range(10))]", "null", "[[2]]"),
        ("isempty(empty), isempty(1, error(\"x\"))", "null", "[true,false]"),
        ("[.[] as [$a] ?// $a | $a]", "[[1],2]", "[[1,2]]"),
        (". as {a: $x, b: [$y, $z]} | [$x, $y, $z]", r#"{"a":1,"b":[2,3]}"#, "[[1,2,3]]"),
        (". as {$a, \"b\": $c} | [$a, $c]", r#"{"a":1,"b":2}"#, "[[1,2]]"),
        ("def f: . * 2; def g(x): x + 1; def h($n): $n * 10; [f, g(.), h(3)]", "4", "[[8,5,30]]"),
        ("def fac: if . <= 1 then 1 else . * (. - 1 | fac) end; fac", "10", "[3628800]"),
        ("def f(g): [g, g]; f(.a)", r#"{"a":1}"#, "[[1,1]]"),
        ("def addvalue($f): map(. + $f); addvalue(.[0])", "[[1,2],[10,20]]", "[[[1,2,1,2],[10,20,1,2]]]"),
        ("[.[] | {a, b: .c}]", r#"[{"a":1,"c":2}]"#, r#"[[{"a":1,"b":2}]]"#),
        ("{(.a, .b): 1}", r#"{"a":"x","b":"y"}"#, r#"[{"x":1},{"y":1}]"#),
        ("{\"a b\": 1, $__loc__}", "null", r#"[{"a b":1,"__loc__":{"file":"<stdin>","line":1}}]"#),
        ("1 as $x | {$x}", "null", r#"[{"x":1}]"#),
        ("\"x\\(1 + 2)y\\(.a)\"", r#"{"a":"z"}"#, r#"["x3yz"]"#),
        ("@base64 \"v=\\(.)\"", r#""hi""#, r#"["v=aGk="]"#),
        ("[until(. > 100; . * 2)]", "1", "[[128]]"),
        ("[while(. < 20; . * 2)]", "1", "[[1,2,4,8,16]]"),
        ("[recurse(if . < 3 then . + 1 else empty end)]", "0", "[[0,1,2,3]]"),
        ("[recurse(.[]?; . != 2)]", "[1,2]", "[[[1,2],1]]"),
        ("[.[] | numbers, strings]", r#"[1,"a",null]"#, r#"[[1,"a"]]"#),
        ("[combinations]", "[[1,2],[3,4]]", "[[[1,3],[1,4],[2,3],[2,4]]]"),
        ("walk(if type == \"number\" then . + 1 else . end)", r#"[1,{"a":2}]"#, r#"[[2,{"a":3}]]"#),
        ("any(.[]; . > 2), all(.[]; . > 0)", "[1,2,3]", "[true,true]"),
        ("IN(2, 3)", "3", "[true]"),
        ("INDEX(.id)", r#"[{"id":1},{"id":2}]"#, r#"[{"1":{"id":1},"2":{"id":2}}]"#),
        ("getpath([\"a\"]) as [$x] | $x", r#"{"a":[9]}"#, "[9]"),
        ("[splits(\", *\")]", r#""a, b,c""#, r#"[["a","b","c"]]"#),
        ("input_line_number", "null", "[0]"),
        ("[.[] | tostring]", r#"[1,"a",[1],{"a":1.5}]"#, r#"[["1","a","[1]","{\"a\":1.5}"]]"#),
        ("[.[] | tojson]", r#"[1,"a",null]"#, r#"[["1","\"a\"","null"]]"#),
        ("ascii", "65", r#"["A"]"#),
        ("[limit(0; 1, 2)]", "null", "[[]]"),
        ("[.[] | select(type == \"object\")] | length", r#"[{},1,{}]"#, "[2]"),
        ("$ENV | type", "null", r#"["object"]"#),
        ("env | type", "null", r#"["object"]"#),
        ("[.[] | abs]", "[-1, 2.5, -0.5]", "[[1,2.5,0.5]]"),
        ("toarray", "1", "[[1]]"),
    ]);
}

#[test]
fn functions() {
    check(&[
        ("length", r#""héllo""#, "[5]"),
        ("utf8bytelength", r#""héllo""#, "[6]"),
        ("[.[] | length]", r#"[null, -5, [1], {"a":1}]"#, "[[0,5,1,1]]"),
        ("keys, keys_unsorted", r#"{"b":1,"a":2}"#, r#"[["a","b"],["b","a"]]"#),
        ("has(\"a\"), has(\"z\")", r#"{"a":1}"#, "[true,false]"),
        ("map(has(1))", "[[0,1],[0]]", "[[true,false]]"),
        ("map(in({\"a\":1}))", r#"["a","b"]"#, "[[true,false]]"),
        ("contains({a:[1]})", r#"{"a":[1,2],"b":3}"#, "[true]"),
        ("contains(\"bar\")", r#""foobar""#, "[true]"),
        ("inside([1,2,3])", "[1,3]", "[true]"),
        ("indices(\", \")", r#""a, b, c""#, "[[1,4]]"),
        ("indices(1)", "[0,1,2,1]", "[[1,3]]"),
        ("indices([1,2])", "[0,1,2,1,2]", "[[1,3]]"),
        ("index(\",\"), rindex(\",\")", r#""a,b,c""#, "[1,3]"),
        ("add", r#"["a","b"]"#, r#"["ab"]"#),
        ("add", "[]", "[null]"),
        ("add(.[].a)", r#"[{"a":1},{"a":2}]"#, "[3]"),
        ("any, all", "[true,false]", "[true,false]"),
        ("flatten, flatten(1)", "[1,[2,[3]]]", "[[1,2,3],[1,2,[3]]]"),
        ("[.[] | floor, ceil, round]", "[1.5]", "[[1,2,2]]"),
        ("sqrt, pow(.; 2), log10", "100", "[10,10000,2]"),
        ("[infinite, -infinite, nan] | map(isinfinite)", "null", "[[true,true,false]]"),
        ("nan | isnan", "null", "[true]"),
        ("sort", r#"[3,"a",null,true,[1],{"a":1},false,1.5]"#, r#"[[null,false,true,1.5,3,"a",[1],{"a":1}]]"#),
        ("sort_by(.a, .b)", r#"[{"a":2,"b":1},{"a":1,"b":2},{"a":1,"b":1}]"#, r#"[[{"a":1,"b":1},{"a":1,"b":2},{"a":2,"b":1}]]"#),
        ("group_by(.a) | map(length)", r#"[{"a":1},{"a":2},{"a":1}]"#, "[[2,1]]"),
        ("unique_by(length)", r#"["a","bb","c","dd"]"#, r#"[["a","bb"]]"#),
        ("min_by(.a), max_by(.a)", r#"[{"a":2},{"a":1},{"a":3}]"#, r#"[{"a":1},{"a":3}]"#),
        ("min, max", "[5,2,9]", "[2,9]"),
        ("min", "[]", "[null]"),
        ("reverse", r#""abc""#, r#"["cba"]"#),
        ("unique", "[3,1,3,2]", "[[1,2,3]]"),
        ("transpose", "[[1,2],[3]]", "[[[1,3],[2,null]]]"),
        ("startswith(\"ab\"), endswith(\"yz\")", r#""abxyz""#, "[true,true]"),
        ("ltrimstr(\"a\"), rtrimstr(\"z\"), ltrimstr(1)", r#""abz""#, r#"["bz","ab","abz"]"#),
        ("trim, ltrim, rtrim", r#""  x  ""#, r#"["x","x  ","  x"]"#),
        ("explode, (explode | implode)", r#""aé""#, r#"[[97,233],"aé"]"#),
        ("split(\", \")", r#""a, b""#, r#"[["a","b"]]"#),
        ("split(\"a+\"; \"g\")", r#""xaaybz""#, r#"[["x","ybz"]]"#),
        ("join(\"-\")", r#"["a",1,null,true]"#, r#"["a-1--true"]"#),
        ("ascii_downcase, ascii_upcase", r#""aBc""#, r#"["abc","ABC"]"#),
        ("tonumber", r#""1.5""#, "[1.5]"),
        ("[.[] | type]", r#"[null,true,1,"a",[],{}]"#, r#"[["null","boolean","number","string","array","object"]]"#),
        ("fromjson", r#""{\"a\":[1,2]}""#, r#"[{"a":[1,2]}]"#),
        ("tojson", r#"{"a":[1,"x"]}"#, r#"["{\"a\":[1,\"x\"]}"]"#),
        ("[splits(\"a\")]", r#""xay""#, r#"[["x","y"]]"#),
        ("getpath([\"a\",0,\"b\"])", r#"{"a":[{"b":5}]}"#, "[5]"),
        ("[paths(type == \"number\")]", r#"{"a":1,"b":[2]}"#, r#"[[["a"],["b",0]]]"#),
        ("builtins | length > 100", "null", "[true]"),
        ("[\"a\", \"b\"] | map(ascii_upcase) | join(\",\")", "null", r#"["A,B"]"#),
        ("isvalid(error), isvalid(1)", "null", "[false,true]"),
        ("frexp, modf", "8.5", "[[0.53125,4],[0.5,8]]"),
        ("significand, logb", "8", "[1,3]"),
        ("[.[] | significand]", "[0, 10]", "[[0,1.25]]"),
        ("atan2(1; 1) * 4 | . * 1000 | floor", "null", "[3141]"),
    ]);
}

#[test]
fn regex_and_formats() {
    check(&[
        ("test(\"FOO\"; \"i\")", r#""xfoo""#, "[true]"),
        ("test(\"a.c\"; \"x\")", r#""abc""#, "[true]"),
        ("[match(\"(a)(?<n>b)?\"; \"g\") | .captures | map(.string)]", r#""ab a""#, r#"[[["a","b"],["a",null]]]"#),
        ("match(\"(?<x>o+)\")", r#""foo""#, r#"[{"offset":1,"length":2,"string":"oo","captures":[{"offset":1,"length":2,"string":"oo","name":"x"}]}]"#),
        ("[match(\"\"; \"g\")] | length", r#""ab""#, "[3]"),
        ("[match(\"\"; \"gn\")] | length", r#""ab""#, "[0]"),
        ("capture(\"(?<a>[a-z]+)-(?<n>[0-9]+)\")", r#""xyz-123""#, r#"[{"a":"xyz","n":"123"}]"#),
        ("[scan(\"c\")], [scan(\"(a)(b)\")]", r#""abcabc""#, r#"[["c","c"],[["a","b"],["a","b"]]]"#),
        ("sub(\"(?<x>[a-z])\"; \"<\\(.x)>\")", r#""ab""#, r#"["<a>b"]"#),
        ("gsub(\"(?<x>[a-z])\"; \"\\(.x|ascii_upcase)\")", r#""ab1c""#, r#"["AB1C"]"#),
        ("gsub(\"\\\\s+\"; \" \")", r#""a   b  c""#, r#"["a b c"]"#),
        ("[sub(\"a\"; \"b\", \"c\")]", r#""a""#, r#"[["b","c"]]"#),
        ("gsub(\"A\"; \"x\"; \"i\")", r#""aA""#, r#"["xx"]"#),
        ("test([\"A\", \"i\"])", r#""a""#, "[true]"),
        ("@csv", r#"[1,"a\"b",null,true]"#, r#"["1,\"a\"\"b\",,true"]"#),
        ("@tsv", r#"["a\tb","c\\d",1]"#, r#"["a\\tb\tc\\\\d\t1"]"#),
        ("@html", r#""<a href='x'>&\"""#, r#"["&lt;a href=&#39;x&#39;&gt;&amp;&quot;"]"#),
        ("@uri", r#""a b&c=é~""#, r#"["a%20b%26c%3D%C3%A9~"]"#),
        ("@sh", r#""it's""#, r#"["'it'\\''s'"]"#),
        ("@sh", r#"["a b", 1]"#, r#"["'a b' 1"]"#),
        ("@base64, (@base64 | @base64d)", r#""hello world""#, r#"["aGVsbG8gd29ybGQ=","hello world"]"#),
        ("@base32, (@base32 | @base32d)", r#""foobar""#, r#"["MZXW6YTBOI======","foobar"]"#),
        ("@json, @text", r#"[1,"a"]"#, r#"["[1,\"a\"]","[1,\"a\"]"]"#),
        ("@json \"x=\\(.)\"", r#""q""#, r#"["x=\"q\""]"#),
        ("format(\"csv\")", "[1,2]", r#"["1,2"]"#),
    ]);
}

#[test]
fn dates() {
    check(&[
        ("todate", "1425599621", r#"["2015-03-05T23:53:41Z"]"#),
        ("fromdate", r#""2015-03-05T23:51:47Z""#, "[1425599507]"),
        ("gmtime", "1425599621", "[[2015,2,5,23,53,41,4,63]]"),
        ("gmtime | mktime", "1425599621", "[1425599621]"),
        ("strptime(\"%Y-%m-%dT%H:%M:%SZ\")", r#""2015-03-05T23:51:47Z""#, "[[2015,2,5,23,51,47,4,63]]"),
        ("strftime(\"%A, %B %d, %Y %j %a %b %e %I%p %u %w\")", "1425599621", r#"["Thursday, March 05, 2015 064 Thu Mar  5 11PM 4 4"]"#),
        ("strftime(\"%G-W%V %U %W %c %D %T %s %%\")", "1425599621", r#"["2015-W10 09 09 Thu Mar  5 23:53:41 2015 03/05/15 23:53:41 1425599621 %"]"#),
        ("strptime(\"%d %b %Y %H:%M\") | mktime", r#""05 Mar 2015 23:53""#, "[1425599580]"),
        ("strptime(\"%s\") | todate", r#""1425599621""#, r#"["2015-03-05T23:53:41Z"]"#),
        ("dateadd(\"seconds\"; 60) | todate", "0", r#"["1970-01-01T00:01:00Z"]"#),
        ("[.[] | todate]", "[0, 951782400]", r#"[["1970-01-01T00:00:00Z","2000-02-29T00:00:00Z"]]"#),
        ("now | type", "null", r#"["number"]"#),
        ("gmtime | todate", "1.5", r#"["1970-01-01T00:00:01Z"]"#),
        ("strftime(\"%V %G\")", "1104537600", r#"["53 2004"]"#),
    ]);
}

#[test]
fn errors_like_jq() {
    check_err(&[
        (".a", "1", "Cannot index number with \"a\""),
        (".[0]", "{}", "Cannot index object with number"),
        (".[]", "1", "Cannot iterate over number (1)"),
        (".[]", "null", "Cannot iterate over null"),
        ("1 + \"a\"", "null", "number (1) and string (\"a\") cannot be added"),
        ("{} - 1", "null", "object ({}) and number (1) cannot be subtracted"),
        ("1 / 0", "null", "cannot be divided because the divisor is zero"),
        ("keys", "1", "number (1) has no keys"),
        ("length", "true", "boolean (true) has no length"),
        ("error(\"custom\")", "null", "custom"),
        ("error({a:1})", "null", "{\"a\":1} (not a string)"),
        ("nosuch", "null", "nosuch/0 is not defined"),
        ("$x", "null", "$x is not defined"),
        ("break $x", "null", "$*label-x is not defined"),
        ("[.[] | {(.): 1}]", "[1]", "Object keys must be strings"),
        ("test(\"(\")", r#""a""#, "is not a valid regex"),
        ("test(\"a\"; \"q\")", r#""a""#, "is not a valid modifier string"),
        ("strptime(\"%Y\")", r#""x""#, "does not match format"),
        ("input", "null", "No more inputs"),
        ("1 == 1 == 1", "null", "not associative"),
        ("@unknown", "null", "is not a valid format"),
        ("\"x\" | ascii_downcase | implode", "null", "implode input must be an array"),
        ("[1] | @csv | @csv", "null", "cannot be csv-formatted"),
        ("{a:{b:1}} | .a.b.c", "null", "Cannot index number with \"c\""),
        ("try error(\"x\") catch error(\"y: \" + .)", "null", "y: x"),
    ]);
}

#[test]
fn try_does_not_catch_downstream_errors() {
    // `try` only covers its own body: an error raised after it must propagate.
    let e = Jq::compile("(try 1) | error(\"after\")").unwrap().outputs(&Value::Null).unwrap_err();
    assert!(e.message.contains("after"));
    let e = Jq::compile(".[] | (. // 0) | error(\"down\")").unwrap().outputs(&Value::from("[1]")).unwrap_err();
    assert!(e.message.contains("down"));
    // break crosses try
    assert_eq!(query(&Value::Null, "[label $f | try (1, break $f, 2) catch 9]").unwrap().to_json(), "[1]");
}

#[test]
fn deep_iteration_is_iterative() {
    assert_eq!(query(&Value::Null, "reduce range(0; 100000) as $i (0; . + $i)").unwrap(), Value::Int(4_999_950_000));
    assert_eq!(query(&Value::Null, "[limit(5000; repeat(1))] | length").unwrap(), Value::Int(5000));
    assert_eq!(query(&Value::Int(0), "until(. >= 20000; . + 1)").unwrap(), Value::Int(20000));
    assert_eq!(query(&Value::Null, "[range(10000)] | map(. * 2) | add").unwrap(), Value::Int(99_990_000));
}

#[test]
fn camel_functions() {
    let mut ex = Exchange::default();
    ex.message.set_header("h", "v");
    ex.set_property("p", Value::Int(2));
    let jq = Jq::compile("[header(\"h\"), header(\"missing\"; \"d\"), property(\"p\"), property(\"nope\")]").unwrap();
    assert_eq!(jq.evaluate_with(&Value::Null, Some(&ex)).unwrap().to_json(), r#"["v","d",2,null]"#);
}
