//! Table-driven XQuery tests.

use super::*;

const BOOKS: &str = r#"<bookstore xmlns:p="urn:pub">
  <book category="cooking" year="2005"><title lang="en">Everyday Italian</title><author>Giada De Laurentiis</author><price>30.00</price></book>
  <book category="children" year="2005"><title lang="en">Harry Potter</title><author>J K. Rowling</author><price>29.99</price></book>
  <book category="web" year="2003"><title lang="en">XQuery Kick Start</title><author>James McGovern</author><author>Per Bothner</author><price>49.99</price></book>
  <book category="web" year="2003"><title lang="fr">Learning XML</title><author>Erik T. Ray</author><price>39.95</price><p:publisher>O'Reilly</p:publisher></book>
</bookstore>"#;

fn run(q: &str) -> Result<String, XqError> {
    let xq = XQuery::compile(q, &[])?;
    xq.evaluate(Some(&Value::from(BOOKS)), &|_, _| None, |d, items| serialize_items(d, items))
}

#[test]
fn table() {
    let cases: &[(&str, &str)] = &[
        // literals, arithmetic, sequences
        ("1 + 2", "3"),
        ("10 div 4", "2.5"),
        ("10 idiv 3", "3"),
        ("10 mod 3", "1"),
        ("-7 idiv 2", "-3"),
        ("0.1 + 0.2", "0.3"),
        ("2.5e0 * 2", "5"),
        ("1e7", "1.0E7"),
        ("(1, 2, 3)", "1 2 3"),
        ("1 to 5", "1 2 3 4 5"),
        ("5 to 1", ""),
        ("count(1 to 10)", "10"),
        ("sum(1 to 100)", "5050"),
        ("avg((1, 2, 3, 4))", "2.5"),
        ("max((3, 7, 2))", "7"),
        ("min(('b', 'a', 'c'))", "a"),
        ("reverse(1 to 3)", "3 2 1"),
        ("subsequence((1,2,3,4,5), 2, 3)", "2 3 4"),
        ("distinct-values((1, 2, 1, 3, 2))", "1 2 3"),
        ("index-of((10, 20, 10), 10)", "1 3"),
        ("insert-before((1,2), 2, 9)", "1 9 2"),
        ("remove((1,2,3), 2)", "1 3"),
        ("empty(())", "true"),
        ("exists(())", "false"),
        ("(1, 2) = (2, 3)", "true"),
        ("(1, 2) != (1, 2)", "true"),
        ("1 eq 1", "true"),
        ("'a' lt 'b'", "true"),
        ("() eq 1", ""),
        // strings
        ("concat('a', 'b', 'c')", "abc"),
        ("string-join(('a', 'b'), '-')", "a-b"),
        ("'a' || 'b' || 1", "ab1"),
        ("substring('12345', 2, 3)", "234"),
        ("substring('12345', 1.5, 2.6)", "234"),
        ("substring-before('2024-01-02', '-')", "2024"),
        ("substring-after('2024-01-02', '-')", "01-02"),
        ("upper-case('abc')", "ABC"),
        ("lower-case('ABC')", "abc"),
        ("normalize-space('  a   b  ')", "a b"),
        ("translate('abc', 'ab', 'AB')", "ABc"),
        ("string-length('héllo')", "5"),
        ("contains('camel', 'me')", "true"),
        ("starts-with('camel', 'ca')", "true"),
        ("ends-with('camel', 'el')", "true"),
        ("tokenize('a,b,,c', ',')", "a b  c"),
        ("tokenize('  a b  ')", "a b"),
        ("replace('abracadabra', 'bra', '*')", "a*cada*"),
        ("replace('2024-01-02', '(\\d+)-(\\d+)-(\\d+)', '$3/$2/$1')", "02/01/2024"),
        ("matches('Camel', '^c', 'i')", "true"),
        ("matches('abc', '^b')", "false"),
        ("string-to-codepoints('AB')", "65 66"),
        ("codepoints-to-string((72, 105))", "Hi"),
        ("compare('a', 'b')", "-1"),
        // numbers
        ("round(2.5)", "3"),
        ("round(-2.5)", "-2"),
        ("round-half-to-even(2.5)", "2"),
        ("floor(2.7)", "2"),
        ("ceiling(2.1)", "3"),
        ("abs(-3)", "3"),
        ("number('12.5') + 1", "13.5"),
        ("number('x')", "NaN"),
        ("boolean('')", "false"),
        ("not(())", "true"),
        // types
        ("5 instance of xs:integer", "true"),
        ("5 instance of xs:decimal", "true"),
        ("5.0 instance of xs:integer", "false"),
        ("'5' cast as xs:integer + 1", "6"),
        ("xs:integer('42') * 2", "84"),
        ("'abc' castable as xs:integer", "false"),
        ("xs:date('2024-03-01') lt xs:date('2024-04-01')", "true"),
        ("year-from-date(xs:date('2024-03-01'))", "2024"),
        ("month-from-dateTime(xs:dateTime('2024-03-01T10:20:30Z'))", "3"),
        ("xs:boolean('1')", "true"),
        ("(1, 'a') instance of xs:anyAtomicType+", "true"),
        ("() instance of empty-sequence()", "true"),
        ("<a/> instance of element(a)", "true"),
        ("<a/> instance of element(b)", "false"),
        // conditionals, quantifiers
        ("if (1 lt 2) then 'yes' else 'no'", "yes"),
        ("some $x in (1, 2, 3) satisfies $x gt 2", "true"),
        ("every $x in (1, 2, 3) satisfies $x gt 2", "false"),
        ("some $x in (1,2), $y in (2,3) satisfies $x = $y", "true"),
        // FLWOR
        ("for $i in 1 to 3 return $i * $i", "1 4 9"),
        ("for $i at $p in ('a', 'b') return concat($p, $i)", "1a 2b"),
        ("for $i in 1 to 10 where $i mod 3 = 0 return $i", "3 6 9"),
        ("let $x := 5, $y := $x * 2 return $y", "10"),
        ("for $x in (3, 1, 2) order by $x descending return $x", "3 2 1"),
        ("for $x in ('b', 'a', 'c') order by $x return $x", "a b c"),
        ("for $x in (1, 2), $y in ('a', 'b') return concat($x, $y)", "1a 1b 2a 2b"),
        ("for $b in /bookstore/book order by number($b/price) return string($b/title)", "Harry Potter Everyday Italian Learning XML XQuery Kick Start"),
        ("for $b in //book where $b/price > 35 return data($b/title)", "XQuery Kick Start Learning XML"),
        ("for $b in //book let $n := count($b/author) where $n > 1 return string($b/title)", "XQuery Kick Start"),
        ("for $b in //book group by $c := string($b/@category) order by $c return concat($c, '=', count($b))", "children=1 cooking=1 web=2"),
        ("for $x in (5, 6, 7) count $c return $c", "1 2 3"),
        ("for $y in distinct-values(//book/@year) order by $y return $y", "2003 2005"),
        // paths
        ("count(//book)", "4"),
        ("string(/bookstore/book[2]/title)", "Harry Potter"),
        ("string(//book[last()]/title)", "Learning XML"),
        ("//book[@category='web'][1]/title/string()", "XQuery Kick Start"),
        ("data(//book/@year)[1]", "2005"),
        ("//title[@lang='fr']/text()", "Learning XML"),
        ("count(//book/author)", "5"),
        ("count(//book[author = 'Per Bothner'])", "1"),
        ("sum(//price)", "149.93"),
        ("declare namespace p = 'urn:pub'; string(//p:publisher)", "O'Reilly"),
        ("count(//*:publisher)", "1"),
        ("count(/bookstore/book/(title|price))", "8"),
        ("//book[1]/title/../@year/string()", "2005"),
        ("count(//book[1]/following-sibling::book)", "3"),
        ("name(//book[1]/*[1])", "title"),
        ("count(//book[1]/descendant::text())", "3"),
        ("(//book)[3]/author[2]/string()", "Per Bothner"),
        ("//book[price > 40]/title ! upper-case(.)", "XQUERY KICK START"),
        ("count(//book except //book[@category='web'])", "2"),
        ("count(//book intersect //book[@year='2003'])", "2"),
        ("//book[1]/title is (//title)[1]", "true"),
        ("//book[1] << //book[2]", "true"),
        ("count(//book/attribute::*)", "8"),
        ("count(//node()[self::text()][normalize-space(.) != ''])", "14"),
        // constructors
        ("<a>{1 + 1}</a>", "<a>2</a>"),
        ("<a x=\"{1+2}\" y='b'/>", "<a x=\"3\" y=\"b\"/>"),
        ("<a>{1, 2}{3}</a>", "<a>1 23</a>"),
        ("<a>  <b/>  </a>", "<a><b/></a>"),
        ("<a>x {{y}} &amp; &#65;</a>", "<a>x {y} &amp; A</a>"),
        ("<a><![CDATA[<b>]]></a>", "<a>&lt;b&gt;</a>"),
        ("element e { attribute x { 'v' }, 'text' }", "<e x=\"v\">text</e>"),
        ("element { concat('n', 1) } { }", "<n1/>"),
        ("<r>{ for $i in 1 to 3 return <i n='{$i}'/> }</r>", "<r><i n=\"1\"/><i n=\"2\"/><i n=\"3\"/></r>"),
        ("<r>{//book[1]/title}</r>", "<r><title xmlns:p=\"urn:pub\" lang=\"en\">Everyday Italian</title></r>"),
        ("<r>{//book[1]/@year}</r>", "<r year=\"2005\"/>"),
        ("<p:x xmlns:p='urn:x'><p:y/></p:x>", "<p:x xmlns:p=\"urn:x\"><p:y/></p:x>"),
        ("<x xmlns='urn:d'><y/></x>", "<x xmlns=\"urn:d\"><y/></x>"),
        ("text { 'a', 'b' }", "a b"),
        ("<!-- hi -->", "<!-- hi -->"),
        ("document { <a/> }/a instance of element(a)", "true"),
        ("<a><b>1</b><b>2</b></a>/b[2]/string()", "2"),
        ("sum(<a><b>1</b><b>2</b></a>/b)", "3"),
        ("count(<a>{(<b/>, <c/>)}</a>/*)", "2"),
        ("string(<a>x<b>y</b>z</a>)", "xyz"),
        ("<a>{ 'x' }</a> = 'x'", "true"),
        // typeswitch, try/catch
        ("typeswitch (5) case xs:string return 's' case xs:integer return 'i' default return 'd'", "i"),
        ("typeswitch (<a/>) case $e as element() return name($e) default return 'd'", "a"),
        ("try { 1 div 0 } catch * { 'caught' }", "caught"),
        ("try { error(xs:QName('err:MY01'), 'boom') } catch err:MY01 { $err:description }", "boom"),
        ("try { xs:integer('x') } catch err:FORG0001 { 'bad' }", "bad"),
        // prolog
        ("declare function local:fact($n as xs:integer) as xs:integer { if ($n le 1) then 1 else $n * local:fact($n - 1) }; local:fact(10)", "3628800"),
        ("declare function local:sum($s) { if (empty($s)) then 0 else head($s) + local:sum(tail($s)) }; local:sum(1 to 2000)", "2001000"),
        ("declare variable $x := 3; declare variable $y as xs:integer := $x * 2; $y", "6"),
        ("declare namespace q = 'urn:q'; <q:a/>", "<q:a xmlns:q=\"urn:q\"/>"),
        ("declare default element namespace 'urn:pub'; count(/bookstore) + count(/*)", "1"),
        ("declare variable $v external := 'dflt'; $v", "dflt"),
        ("xquery version '1.0'; (: comment (: nested :) :) 1", "1"),
        ("declare boundary-space preserve; <a> </a>", "<a> </a>"),
        ("declare function local:f() { <x/> }; count((local:f(), local:f()))", "2"),
        // misc
        ("deep-equal(<a x='1'><b/></a>, <a x='1'><b/></a>)", "true"),
        ("deep-equal((1, 2), (1, 3))", "false"),
        ("string(current-dateTime()) castable as xs:dateTime", "true"),
        ("math:pow(2, 10)", "1024"),
        ("serialize(<a>b</a>)", "&lt;a&gt;b&lt;/a&gt;"),
        ("parse-xml('<x>1</x>')/x + 1", "2"),
        ("(1 to 10)[. mod 2 = 0]", "2 4 6 8 10"),
        ("(1 to 10)[position() = last()]", "10"),
        ("for $w in tokenize('the quick brown fox', ' ') where string-length($w) > 3 return $w", "quick brown"),
        ("string((//title)[1]/@lang) = 'en'", "true"),
        ("//book[@year = 2005][2]/price/xs:decimal(.) * 2", "59.98"),
    ];
    let mut failures = Vec::new();
    for (q, want) in cases {
        match run(q) {
            Ok(got) if got == *want => {}
            Ok(got) => failures.push(format!("{}\n   want: {}\n   got:  {}", q, want, got)),
            Err(e) => failures.push(format!("{}\n   want: {}\n   error: {}", q, want, e)),
        }
    }
    assert!(cases.len() >= 60);
    assert!(failures.is_empty(), "{} of {} cases failed:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

#[test]
fn errors() {
    let cases: &[(&str, &str)] = &[
        ("1 +", "XPST0003"),
        ("$undefined", "XPST0008"),
        ("1 div 0", "FOAR0001"),
        ("'a' + 1", "XPTY0004"),
        ("xs:integer('x')", "FORG0001"),
        ("unknown-fn(1)", "XPST0017"),
        ("(1, 2) eq 1", "XPTY0004"),
        ("<a>{attribute x {1}}</a>, <a>t{attribute x {1}}</a>", "XQTY0024"),
        ("error()", "FOER0000"),
        ("exactly-one((1, 2))", "FORG0005"),
        ("declare variable $x external; $x", "XPDY0002"),
        ("declare function local:f($a as xs:integer) { $a }; local:f('x')", "XPTY0004"),
        ("<a></b>", "XPST0003"),
        ("p:x", "XPST0081"),
        ("declare function local:loop($n) { local:loop($n + 1) }; local:loop(1)", "XPDY0130"),
    ];
    for (q, code) in cases {
        let e = run(q).expect_err(q);
        assert_eq!(e.code, *code, "{}: {}", q, e);
    }
}

#[test]
fn external_variables_and_results() {
    let q = XQuery::compile("declare variable $in.headers.foo external; declare variable $doc as element() external; declare variable $n as xs:integer external; concat($in.headers.foo, '-', $doc/@id, '-', $n + 1)", &[]).unwrap();
    let ext = q.external_variables();
    assert_eq!(ext.len(), 3);
    assert!(ext[1].wants_nodes);
    let out = q
        .evaluate(
            None,
            &|_, l| match l {
                "in.headers.foo" => Some(Value::from("bar")),
                "doc" => Some(Value::from("<d id='7'/>")),
                "n" => Some(Value::from("41")),
                _ => None,
            },
            |d, i| items_to_string(d, i),
        )
        .unwrap();
    assert_eq!(out, "bar-7-42");
    // list conversion keeps atomic types
    let q = XQuery::compile("(1, 2.5, 'x', true(), <a>b</a>, <a b='c'/>/@b)", &[]).unwrap();
    let v = q.evaluate(None, &|_, _| None, |d, i| items_to_list(d, i)).unwrap();
    assert_eq!(v, Value::Array(vec![Value::Int(1), Value::Float(2.5), Value::from("x"), Value::Bool(true), Value::from("<a>b</a>"), Value::from("c")]));
    // the context item is absent when the input is not XML
    let q = XQuery::compile("/a", &[]).unwrap();
    let e = q.evaluate(Some(&Value::from("not xml")), &|_, _| None, |_, _| ()).unwrap_err();
    assert_eq!(e.code, "XPDY0002");
    assert!(e.message.contains("not well-formed"), "{}", e);
    // namespace bindings
    let q = XQuery::compile("string(/x:a)", &[("x".into(), "urn:x".into())]).unwrap();
    assert_eq!(q.evaluate(Some(&Value::from("<a xmlns='urn:x'>1</a>")), &|_, _| None, |d, i| items_to_string(d, i)).unwrap(), "1");
}

#[test]
fn doc_function_reads_local_files() {
    let dir = std::env::temp_dir().join(format!("camel-xquery-doc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("data.xml"), "<items><i>1</i><i>2</i></items>").unwrap();
    std::fs::write(dir.join("q.xq"), "sum(doc('data.xml')//i)").unwrap();
    let q = XQuery::compile_file(&dir.join("q.xq"), &[]).unwrap();
    assert_eq!(q.evaluate(None, &|_, _| None, |d, i| serialize_items(d, i)).unwrap(), "3");
    let q = XQuery::compile("doc('http://example.com/x.xml')", &[]).unwrap();
    assert_eq!(q.evaluate(None, &|_, _| None, |_, _| ()).unwrap_err().code, "FODC0002");
    std::fs::remove_dir_all(&dir).ok();
}
