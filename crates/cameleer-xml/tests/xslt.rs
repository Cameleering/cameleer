//! XSLT 1.0 transformations.

use cameleer_core::value::Value;
use cameleer_xml::xslt::{transform, Stylesheet};

const XSL_HEAD: &str = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">"#;

fn xsl(body: &str) -> String {
    format!("{}<xsl:output omit-xml-declaration=\"yes\"/>{}</xsl:stylesheet>", XSL_HEAD, body)
}

fn run(body: &str, xml: &str) -> String {
    transform(&xsl(body), xml).unwrap_or_else(|e| panic!("{}: {}", e.kind, e.message))
}

const ORDERS: &str = r#"<orders>
  <order id="3" customer="acme"><item sku="a" qty="2" price="1.5"/><item sku="b" qty="1" price="10"/></order>
  <order id="1" customer="globex"><item sku="c" qty="5" price="2"/></order>
  <order id="2" customer="acme"><item sku="a" qty="1" price="1.5"/></order>
</orders>"#;

#[test]
fn identity_transform() {
    let x = r#"<a xmlns:p="urn:p" x="1"><!--c--><p:b>t &amp; u</p:b><?pi d?><c/></a>"#;
    let out = run(r#"<xsl:template match="@*|node()"><xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy></xsl:template>"#, x);
    assert_eq!(out, x.replace("<c/>", "<c/>"));
}

#[test]
fn identity_with_override() {
    let out = run(
        r#"<xsl:template match="@*|node()"><xsl:copy><xsl:apply-templates select="@*|node()"/></xsl:copy></xsl:template>
           <xsl:template match="secret"/>
           <xsl:template match="@id"><xsl:attribute name="ref"><xsl:value-of select="."/></xsl:attribute></xsl:template>"#,
        "<r><a id='1'/><secret>x</secret><b/></r>",
    );
    assert_eq!(out, r#"<r><a ref="1"/><b/></r>"#);
}

#[test]
fn xml_declaration_and_default_output() {
    let out = transform(&format!("{}<xsl:template match='/'><r/></xsl:template></xsl:stylesheet>", XSL_HEAD), "<a/>").unwrap();
    assert_eq!(out, r#"<?xml version="1.0" encoding="UTF-8"?><r/>"#);
}

#[test]
fn value_of_and_for_each() {
    let out = run(r#"<xsl:template match="/"><xsl:for-each select="orders/order"><xsl:value-of select="@id"/>,</xsl:for-each></xsl:template>"#, ORDERS);
    assert_eq!(out, "3,1,2,");
}

#[test]
fn sorting_numeric_text_descending() {
    let out = run(
        r#"<xsl:template match="/"><xsl:for-each select="//order"><xsl:sort select="@id" data-type="number"/><xsl:value-of select="@id"/></xsl:for-each>|<xsl:for-each select="//order"><xsl:sort select="@customer"/><xsl:sort select="@id" order="descending" data-type="number"/><xsl:value-of select="concat(@customer, @id, ' ')"/></xsl:for-each></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "123|acme3 acme2 globex1 ");
}

#[test]
fn sorting_case_order_and_text_vs_number() {
    let x = "<l><i>b</i><i>B</i><i>a</i><i>10</i><i>9</i></l>";
    let out = run(r#"<xsl:template match="/"><xsl:for-each select="//i"><xsl:sort/><xsl:value-of select="."/>,</xsl:for-each></xsl:template>"#, x);
    assert_eq!(out, "10,9,a,B,b,");
    let out = run(r#"<xsl:template match="/"><xsl:for-each select="//i"><xsl:sort case-order="lower-first"/><xsl:value-of select="."/>,</xsl:for-each></xsl:template>"#, x);
    assert_eq!(out, "10,9,a,b,B,");
    let out = run(r#"<xsl:template match="/"><xsl:apply-templates select="//i"><xsl:sort data-type="number" order="descending"/></xsl:apply-templates></xsl:template><xsl:template match="i"><xsl:value-of select="."/>;</xsl:template>"#, x);
    assert_eq!(out, "10;9;b;B;a;");
}

#[test]
fn grouping_with_keys_muenchian() {
    let out = run(
        r#"<xsl:key name="by-customer" match="order" use="@customer"/>
           <xsl:template match="/">
             <xsl:for-each select="//order[generate-id() = generate-id(key('by-customer', @customer)[1])]">
               <xsl:sort select="@customer"/>
               <xsl:value-of select="@customer"/>=<xsl:value-of select="count(key('by-customer', @customer))"/>
               <xsl:text>;</xsl:text>
             </xsl:for-each>
           </xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "acme=2;globex=1;");
}

#[test]
fn key_in_pattern_and_multiple_values() {
    let out = run(
        r#"<xsl:key name="sku" match="item" use="@sku"/>
           <xsl:template match="/"><xsl:value-of select="count(key('sku', 'a'))"/>-<xsl:value-of select="sum(key('sku', //item[@qty=5]/@sku | //item[@qty=2]/@sku)/@qty)"/><xsl:apply-templates select="//item"/></xsl:template>
           <xsl:template match="item"/>
           <xsl:template match="key('sku','b')">[b]</xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "2-8[b]");
}

#[test]
fn apply_templates_modes_and_priorities() {
    let out = run(
        r#"<xsl:template match="/"><xsl:apply-templates select="//order"/>|<xsl:apply-templates select="//order" mode="short"/></xsl:template>
           <xsl:template match="order">O<xsl:value-of select="@id"/></xsl:template>
           <xsl:template match="order[@customer='acme']">A<xsl:value-of select="@id"/></xsl:template>
           <xsl:template match="*" mode="short">s</xsl:template>
           <xsl:template match="order" mode="short" priority="-1">never</xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "A3O1A2|sss");
}

#[test]
fn builtin_templates_copy_text() {
    let out = run(r#"<xsl:template match="b">[<xsl:apply-templates/>]</xsl:template>"#, "<a>x<b>y<c>z</c></b><!--no--><?no?></a>");
    assert_eq!(out, "x[yz]");
}

#[test]
fn call_template_with_params_and_recursion() {
    let out = run(
        r#"<xsl:template match="/"><xsl:call-template name="fact"><xsl:with-param name="n" select="5"/></xsl:call-template>,<xsl:call-template name="greet"/></xsl:template>
           <xsl:template name="fact"><xsl:param name="n"/><xsl:param name="acc" select="1"/>
             <xsl:choose><xsl:when test="$n &lt;= 1"><xsl:value-of select="$acc"/></xsl:when>
             <xsl:otherwise><xsl:call-template name="fact"><xsl:with-param name="n" select="$n - 1"/><xsl:with-param name="acc" select="$acc * $n"/></xsl:call-template></xsl:otherwise></xsl:choose>
           </xsl:template>
           <xsl:template name="greet"><xsl:param name="who">world</xsl:param>hello <xsl:value-of select="$who"/></xsl:template>"#,
        "<x/>",
    );
    assert_eq!(out, "120,hello world");
}

#[test]
fn variables_scoping_and_rtf() {
    let out = run(
        r#"<xsl:variable name="total" select="sum(//item/@qty)"/>
           <xsl:variable name="rtf"><v>1</v><v>2</v></xsl:variable>
           <xsl:template match="/"><xsl:variable name="x" select="'outer'"/>
             <xsl:for-each select="//order[1]"><xsl:variable name="x" select="'inner'"/><xsl:value-of select="$x"/></xsl:for-each>
             <xsl:value-of select="concat(' ', $x, ' ', $total, ' ', $rtf, ' ', count($rtf/v))"/>
             <xsl:copy-of select="$rtf"/>
           </xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "inner outer 9 12 2<v>1</v><v>2</v>");
}

#[test]
fn global_params_from_outside() {
    let ss = Stylesheet::compile(&xsl(r#"<xsl:param name="greeting" select="'hi'"/><xsl:param name="n" select="1"/><xsl:template match="/"><xsl:value-of select="concat($greeting, ':', $n * 2)"/></xsl:template>"#), None).unwrap();
    assert_eq!(ss.transform_to_string("<a/>", &[]).unwrap(), "hi:2");
    assert_eq!(ss.transform_to_string("<a/>", &[("greeting".into(), Value::from("hello")), ("n".into(), Value::Int(21))]).unwrap(), "hello:42");
}

#[test]
fn if_choose() {
    let out = run(
        r#"<xsl:template match="order"><xsl:if test="@customer='acme'">*</xsl:if><xsl:choose><xsl:when test="count(item) &gt; 1">many</xsl:when><xsl:when test="item/@qty &gt; 2">bulk</xsl:when><xsl:otherwise>one</xsl:otherwise></xsl:choose>;</xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out.split_whitespace().collect::<String>(), "*many;bulk;*one;");
}

#[test]
fn element_attribute_comment_pi_text() {
    let out = run(
        r#"<xsl:template match="/"><xsl:element name="{name(/*)}-copy"><xsl:attribute name="n"><xsl:value-of select="count(//order)"/></xsl:attribute><xsl:comment> generated </xsl:comment><xsl:processing-instruction name="app">v="1"</xsl:processing-instruction><xsl:text>  spaced  </xsl:text></xsl:element></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, r#"<orders-copy n="3"><!-- generated --><?app v="1"?>  spaced  </orders-copy>"#);
}

#[test]
fn literal_result_elements_avt_and_namespaces() {
    let out = transform(
        r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:inv="urn:invoice" xmlns:junk="urn:junk" exclude-result-prefixes="junk">
             <xsl:output omit-xml-declaration="yes"/>
             <xsl:template match="/orders"><inv:invoices count="{count(order)}" first="{order[1]/@id}" literal="{{x}}"><xsl:apply-templates select="order"/></inv:invoices></xsl:template>
             <xsl:template match="order"><inv:invoice id="{@id}"/></xsl:template>
           </xsl:stylesheet>"#,
        ORDERS,
    )
    .unwrap();
    assert_eq!(out, r#"<inv:invoices xmlns:inv="urn:invoice" count="3" first="3" literal="{x}"><inv:invoice id="3"/><inv:invoice id="1"/><inv:invoice id="2"/></inv:invoices>"#);
}

#[test]
fn element_with_namespace_attribute() {
    let out = run(r#"<xsl:template match="/"><xsl:element name="x:e" namespace="urn:x"><xsl:attribute name="y:a" namespace="urn:y">1</xsl:attribute></xsl:element></xsl:template>"#, "<a/>");
    assert_eq!(out, r#"<x:e xmlns:x="urn:x" xmlns:y="urn:y" y:a="1"/>"#);
}

#[test]
fn text_output_method() {
    let ss = Stylesheet::compile(
        &format!(
            "{}<xsl:output method=\"text\"/><xsl:template match=\"/\"><xsl:for-each select=\"//order\"><xsl:value-of select=\"concat(@id, '&amp;', @customer)\"/><xsl:text>&#10;</xsl:text></xsl:for-each></xsl:template></xsl:stylesheet>",
            XSL_HEAD
        ),
        None,
    )
    .unwrap();
    let r = ss.transform(cameleer_xml::Document::parse(ORDERS).unwrap(), &[]).unwrap();
    assert_eq!(r.output, "3&acme\n1&globex\n2&acme\n");
    assert_eq!(r.media_type, "text/plain");
}

#[test]
fn html_output_method() {
    let out = transform(
        &format!("{}<xsl:output method=\"html\"/><xsl:template match=\"/\"><html><body><p>a &lt; b</p><br/><input type=\"checkbox\" checked=\"checked\"/></body></html></xsl:template></xsl:stylesheet>", XSL_HEAD),
        "<x/>",
    )
    .unwrap();
    assert_eq!(out, "<html><body><p>a &lt; b</p><br><input type=\"checkbox\" checked></body></html>");
}

#[test]
fn indent_output() {
    let out = transform(
        &format!(
            "{}<xsl:output indent=\"yes\" xmlns:xalan=\"http://xml.apache.org/xslt\" xalan:indent-amount=\"2\"/><xsl:template match=\"/\"><r><a><b>t</b></a><c/></r></xsl:template></xsl:stylesheet>",
            XSL_HEAD
        ),
        "<x/>",
    )
    .unwrap();
    assert_eq!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<r>\n  <a>\n    <b>t</b>\n  </a>\n  <c/>\n</r>");
}

#[test]
fn copy_of_and_copy_attributes() {
    let out = run(
        r#"<xsl:template match="/"><r><xsl:copy-of select="//order[@id=2]/@customer"/><xsl:copy-of select="//order[@id=1]"/><xsl:copy-of select="count(//item)"/><xsl:copy-of select="//order[@id=3]/@id"/></r></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, r#"<r customer="acme"><order id="1" customer="globex"><item sku="c" qty="5" price="2"/></order>4</r>"#);
}

#[test]
fn xsl_number_levels_and_formats() {
    let x = "<book><ch><s/><s/></ch><ch><s/></ch></book>";
    let out = run(
        r#"<xsl:template match="/"><xsl:for-each select="//s"><xsl:number level="multiple" count="ch|s" format="1.a"/>;<xsl:number level="any" format="(i)"/>;</xsl:for-each><xsl:for-each select="//ch"><xsl:number format="A"/></xsl:for-each><xsl:number value="1234567" grouping-separator="," grouping-size="3"/></xsl:template>"#,
        x,
    );
    assert_eq!(out, "1.a;(i);1.b;(ii);2.a;(iii);AB1,234,567");
}

#[test]
fn format_number_and_decimal_format() {
    let out = run(
        r#"<xsl:decimal-format name="eu" decimal-separator="," grouping-separator="."/>
           <xsl:template match="/"><xsl:value-of select="format-number(1234.5, '#,##0.00')"/>|<xsl:value-of select="format-number(1234.5, '#.##0,00', 'eu')"/>|<xsl:value-of select="format-number(0.125, '0.0%')"/>|<xsl:value-of select="format-number(sum(//item/@price), '000.0')"/></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "1,234.50|1.234,50|12.5%|015.0");
}

#[test]
fn strip_and_preserve_space() {
    let x = "<r>\n  <a>  </a>\n  <p xml:space='preserve'> <q/> </p><k> </k></r>";
    let out = run(r#"<xsl:strip-space elements="*"/><xsl:preserve-space elements="k"/><xsl:template match="/"><xsl:value-of select="count(//text())"/></xsl:template>"#, x);
    assert_eq!(out, "3");
}

#[test]
fn attribute_sets_and_use() {
    let out = run(
        r#"<xsl:attribute-set name="base"><xsl:attribute name="class">c</xsl:attribute></xsl:attribute-set>
           <xsl:attribute-set name="more" use-attribute-sets="base"><xsl:attribute name="id">x</xsl:attribute></xsl:attribute-set>
           <xsl:template match="/"><div xsl:use-attribute-sets="more"/><xsl:element name="span" use-attribute-sets="base"/></xsl:template>"#,
        "<a/>",
    );
    assert_eq!(out, r#"<div class="c" id="x"/><span class="c"/>"#);
}

#[test]
fn messages_and_terminate() {
    let ss = Stylesheet::compile(&xsl(r#"<xsl:template match="/"><xsl:message>note <xsl:value-of select="name(*)"/></xsl:message>ok</xsl:template>"#), None).unwrap();
    let r = ss.transform(cameleer_xml::Document::parse("<a/>").unwrap(), &[]).unwrap();
    assert_eq!(r.messages, vec!["note a".to_string()]);
    assert_eq!(r.output, "ok");
    let ss = Stylesheet::compile(&xsl(r#"<xsl:template match="/"><xsl:message terminate="yes">stop here</xsl:message></xsl:template>"#), None).unwrap();
    let e = ss.transform_to_string("<a/>", &[]).unwrap_err();
    assert_eq!(e.kind, "TransformerException");
    assert!(e.message.contains("stop here"));
}

#[test]
fn simplified_stylesheet_syntax() {
    let out = transform(r#"<html xsl:version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><p><xsl:value-of select="count(//order)"/></p></html>"#, ORDERS).unwrap();
    assert_eq!(out, "<html><p>3</p></html>");
}

#[test]
fn include_import_and_document() {
    let dir = std::env::temp_dir().join(format!("cameleer-xml-xslt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("base.xsl"), format!("{}<xsl:template match=\"item\">base</xsl:template><xsl:template match=\"order\">B<xsl:apply-templates/></xsl:template></xsl:stylesheet>", XSL_HEAD)).unwrap();
    std::fs::write(dir.join("inc.xsl"), format!("{}<xsl:template name=\"sep\">|</xsl:template></xsl:stylesheet>", XSL_HEAD)).unwrap();
    std::fs::write(dir.join("lookup.xml"), "<map><e k='acme'>ACME Corp</e><e k='globex'>Globex Inc</e></map>").unwrap();
    std::fs::write(
        dir.join("main.xsl"),
        format!(
            "{}<xsl:import href=\"base.xsl\"/><xsl:include href=\"inc.xsl\"/><xsl:output omit-xml-declaration=\"yes\"/>\
             <xsl:template match=\"/\"><xsl:apply-templates select=\"//order[@id=1]\"/><xsl:call-template name=\"sep\"/><xsl:value-of select=\"document('lookup.xml')/map/e[@k=current()//order[1]/@customer]\"/><xsl:call-template name=\"sep\"/><xsl:value-of select=\"count(document('')/*/xsl:template)\"/></xsl:template>\
             <xsl:template match=\"order\">M[<xsl:apply-imports/>]</xsl:template></xsl:stylesheet>",
            XSL_HEAD
        ),
    )
    .unwrap();
    let ss = Stylesheet::compile_file(&dir.join("main.xsl")).unwrap();
    assert_eq!(ss.transform_to_string(ORDERS, &[]).unwrap(), "M[Bbase]|ACME Corp|2");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn xslt3_features_are_rejected_clearly() {
    let e = Stylesheet::compile(
        r#"<xsl:stylesheet version="3.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template match="/"><xsl:iterate select="//order"><x/></xsl:iterate></xsl:template></xsl:stylesheet>"#,
        None,
    )
    .unwrap_err();
    assert_eq!(e.kind, "TransformerConfigurationException");
    assert!(e.message.contains("xsl:iterate is not supported"), "{}", e.message);
    // XSLT 2.0 instructions are rejected in 1.0 stylesheets
    let e = Stylesheet::compile(&xsl(r#"<xsl:template match="/"><xsl:for-each-group select="x" group-by="y"/></xsl:template>"#), None).unwrap_err();
    assert!(e.message.contains("xsl:for-each-group is not supported"), "{}", e.message);
    // and XPath 2.0 syntax too
    assert!(Stylesheet::compile(&xsl(r#"<xsl:template match="/"><xsl:value-of select="for $i in 1 to 3 return $i"/></xsl:template>"#), None).is_err());
    // xsl:fallback is honoured
    let out = transform(
        r#"<xsl:stylesheet version="3.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output omit-xml-declaration="yes"/><xsl:template match="/"><xsl:iterate select="x"><xsl:fallback>fb</xsl:fallback></xsl:iterate></xsl:template></xsl:stylesheet>"#,
        "<a/>",
    )
    .unwrap();
    assert_eq!(out, "fb");
}

#[test]
fn stylesheet_errors() {
    assert!(Stylesheet::compile("<notxsl/>", None).is_err());
    assert!(Stylesheet::compile(&xsl("<xsl:template match='a[' />"), None).is_err());
    assert!(Stylesheet::compile(&xsl("<xsl:bogus/>"), None).is_err());
    assert!(Stylesheet::compile(&xsl("<xsl:template match='/'><xsl:value-of/></xsl:template>"), None).unwrap_err().message.contains("select"));
    let e = transform(&xsl("<xsl:template match='/'><xsl:call-template name='nope'/></xsl:template>"), "<a/>").unwrap_err();
    assert!(e.message.contains("nope"));
    let e = transform(&xsl("<xsl:template match='/'><xsl:call-template name='r'/></xsl:template><xsl:template name='r'><xsl:call-template name='r'/></xsl:template>"), "<a/>").unwrap_err();
    assert!(e.message.contains("too deep"));
}

#[test]
fn disable_output_escaping_and_cdata_sections() {
    let out = run(r#"<xsl:template match="/"><r><xsl:text disable-output-escaping="yes">&lt;raw/&gt;</xsl:text><xsl:value-of select="'&lt;esc&gt;'"/></r></xsl:template>"#, "<a/>");
    assert_eq!(out, "<r><raw/>&lt;esc&gt;</r>");
    let out = transform(&format!("{}<xsl:output omit-xml-declaration=\"yes\" cdata-section-elements=\"script\"/><xsl:template match=\"/\"><script>a &lt; b</script></xsl:template></xsl:stylesheet>", XSL_HEAD), "<a/>").unwrap();
    assert_eq!(out, "<script><![CDATA[a < b]]></script>");
}

#[test]
fn functions_misc() {
    let out = run(
        r#"<xsl:template match="/"><xsl:value-of select="system-property('xsl:version')"/>,<xsl:value-of select="element-available('xsl:for-each')"/>,<xsl:value-of select="function-available('key')"/>,<xsl:value-of select="function-available('nope')"/>,<xsl:value-of select="generate-id(/*) = generate-id(/orders)"/></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, "1,true,true,false,true");
}

#[test]
fn exslt_node_set() {
    let out = run(
        r#"<xsl:variable name="t"><i>3</i><i>1</i><i>2</i></xsl:variable>
           <xsl:template match="/" xmlns:exsl="http://exslt.org/common"><xsl:for-each select="exsl:node-set($t)/i"><xsl:sort/><xsl:value-of select="."/></xsl:for-each></xsl:template>"#,
        "<a/>",
    );
    assert_eq!(out, "123");
}

#[test]
fn deep_template_recursion_from_a_small_stack() {
    // 4000 nested call-templates from a 2 MiB thread (a route thread): the transformation continues on new
    // stack segments instead of overflowing (no thread is started for shallow stylesheets)
    let ss = Stylesheet::compile(
        &xsl(r#"<xsl:template match="/"><r><xsl:call-template name="r"><xsl:with-param name="n" select="4000"/></xsl:call-template></r></xsl:template>
<xsl:template name="r"><xsl:param name="n"/><xsl:choose><xsl:when test="$n &gt; 0"><xsl:call-template name="r"><xsl:with-param name="n" select="$n - 1"/></xsl:call-template></xsl:when><xsl:otherwise>bottom</xsl:otherwise></xsl:choose></xsl:template>"#),
        None,
    )
    .unwrap();
    let out = std::thread::Builder::new().stack_size(2 << 20).spawn(move || ss.transform_to_string("<a/>", &[]).unwrap()).unwrap().join().unwrap();
    assert_eq!(out, "<r>bottom</r>");
    // and over a deeply nested document through the built-in templates
    let depth = 3000;
    let doc = format!("{}x{}", "<e>".repeat(depth), "</e>".repeat(depth));
    let ss = Stylesheet::compile(&xsl(""), None).unwrap();
    let copy = doc.clone();
    let out = std::thread::Builder::new().stack_size(2 << 20).spawn(move || ss.transform_to_string(&doc, &[]).unwrap()).unwrap().join().unwrap();
    assert_eq!(out, "x");
    // a deeply nested result is serialized too
    let ss = Stylesheet::compile(&xsl(r#"<xsl:template match="/"><xsl:copy-of select="."/></xsl:template>"#), None).unwrap();
    let out = std::thread::Builder::new().stack_size(2 << 20).spawn(move || ss.transform_to_string(&copy, &[]).unwrap()).unwrap().join().unwrap();
    assert_eq!(out.len(), depth * 7 + 1);
}
