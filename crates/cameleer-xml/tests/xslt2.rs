//! XSLT 2.0 core (xslt-saxon) and XSLT 1.0 extras (number letter-value / lang, collations,
//! HTML META).

use cameleer_xml::xslt::{transform, Stylesheet};

fn xsl2(body: &str) -> String {
    format!(
        r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:f="urn:f" exclude-result-prefixes="xs f"><xsl:output omit-xml-declaration="yes"/>{}</xsl:stylesheet>"#,
        body
    )
}

fn run2(body: &str, xml: &str) -> String {
    transform(&xsl2(body), xml).unwrap_or_else(|e| panic!("{}: {}", e.kind, e.message))
}

fn run1(body: &str, xml: &str) -> String {
    let s = format!(r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output omit-xml-declaration="yes"/>{}</xsl:stylesheet>"#, body);
    transform(&s, xml).unwrap_or_else(|e| panic!("{}: {}", e.kind, e.message))
}

const ORDERS: &str = r#"<orders>
  <order id="3" customer="acme" total="5"/>
  <order id="1" customer="globex" total="10"/>
  <order id="2" customer="acme" total="7"/>
  <order id="4" customer="initech" total="1"/>
</orders>"#;

#[test]
fn for_each_group_by_with_sort_and_current_group() {
    let out = run2(
        r#"<xsl:template match="/"><xsl:for-each-group select="//order" group-by="@customer"><xsl:sort select="current-grouping-key()" order="descending"/><g k="{current-grouping-key()}" n="{count(current-group())}" sum="{sum(current-group()/@total)}"><xsl:value-of select="current-group()/@id" separator=","/></g></xsl:for-each-group></xsl:template>"#,
        ORDERS,
    );
    assert_eq!(out, r#"<g k="initech" n="1" sum="1">4</g><g k="globex" n="1" sum="10">1</g><g k="acme" n="2" sum="12">3,2</g>"#);
}

#[test]
fn for_each_group_adjacent_starting_ending() {
    let xml = "<r><h>A</h><p>1</p><p>2</p><h>B</h><p>3</p></r>";
    let out = run2(
        r#"<xsl:template match="/r"><xsl:for-each-group select="*" group-starting-with="h"><s t="{current-group()[1]}"><xsl:value-of select="current-group()[position() gt 1]" separator="+"/></s></xsl:for-each-group></xsl:template>"#,
        xml,
    );
    assert_eq!(out, r#"<s t="A">1+2</s><s t="B">3</s>"#);
    let out = run2(
        r#"<xsl:template match="/r"><xsl:for-each-group select="*" group-adjacent="name()"><xsl:value-of select="concat(current-grouping-key(), count(current-group()))"/>;</xsl:for-each-group></xsl:template>"#,
        xml,
    );
    assert_eq!(out, "h1;p2;h1;p1;");
    let out = run2(
        r#"<xsl:template match="/r"><xsl:for-each-group select="*" group-ending-with="p[. = '2']">[<xsl:value-of select="current-group()" separator=""/>]</xsl:for-each-group></xsl:template>"#,
        xml,
    );
    assert_eq!(out, "[A12][B3]");
}

#[test]
fn functions_sequences_and_typed_variables() {
    let out = run2(
        r#"<xsl:function name="f:fact" as="xs:integer"><xsl:param name="n" as="xs:integer"/><xsl:sequence select="if ($n le 1) then 1 else $n * f:fact($n - 1)"/></xsl:function>
           <xsl:function name="f:wrap"><xsl:param name="s"/><b><xsl:value-of select="$s"/></b></xsl:function>
           <xsl:function name="f:evens" as="xs:integer*"><xsl:param name="max"/><xsl:for-each select="1 to $max"><xsl:if test=". mod 2 = 0"><xsl:sequence select="xs:integer(.)"/></xsl:if></xsl:for-each></xsl:function>
           <xsl:template match="/">
             <xsl:variable name="seq" as="xs:string*"><xsl:sequence select="('x', 'y')"/><xsl:sequence select="'z'"/></xsl:variable>
             <r fact="{f:fact(5)}" evens="{string-join(for $e in f:evens(9) return string($e), ',')}" n="{count($seq)}" avail="{function-available('f:fact')}"><xsl:copy-of select="f:wrap('w')"/><xsl:value-of select="$seq" separator="-"/></r>
           </xsl:template>"#,
        "<a/>",
    );
    assert_eq!(out, r#"<r fact="120" evens="2,4,6,8" n="3" avail="true"><b>w</b>x-y-z</r>"#);
}

#[test]
fn analyze_string_and_regex_group() {
    let out = run2(
        r#"<xsl:template match="/"><xsl:analyze-string select="d" regex="(\d+)-(\d+)"><xsl:matching-substring><n a="{regex-group(1)}" b="{regex-group(2)}"/></xsl:matching-substring><xsl:non-matching-substring><t><xsl:value-of select="."/></t></xsl:non-matching-substring></xsl:analyze-string></xsl:template>"#,
        "<d>x 1-2 y 30-4</d>",
    );
    assert_eq!(out, r#"<t>x </t><n a="1" b="2"/><t> y </t><n a="30" b="4"/>"#);
}

#[test]
fn tunnel_params_and_next_match() {
    let out = run2(
        r#"<xsl:template match="/"><xsl:apply-templates select="r/a"><xsl:with-param name="t" select="'T'" tunnel="yes"/></xsl:apply-templates></xsl:template>
           <xsl:template match="a"><a><xsl:apply-templates/></a></xsl:template>
           <xsl:template match="b"><xsl:param name="t" tunnel="yes"/><xsl:param name="u" select="'none'"/><b t="{$t}" u="{$u}"/></xsl:template>
           <xsl:template match="b[@x]" priority="2"><x><xsl:next-match/></x></xsl:template>"#,
        "<r><a><b/><b x='1'/></a></r>",
    );
    assert_eq!(out, r#"<a><b t="T" u="none"/><x><b t="T" u="none"/></x></a>"#);
}

#[test]
fn result_document_writes_secondary_output() {
    let dir = std::env::temp_dir().join(format!("camel-xslt2-rd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let href = dir.join("out").join("o-{@id}.xml");
    let ss = Stylesheet::compile(
        &xsl2(&format!(
            r#"<xsl:template match="/"><main><xsl:for-each select="//order[@customer='acme']"><xsl:result-document href="{}" omit-xml-declaration="yes"><o><xsl:value-of select="@total"/></o></xsl:result-document></xsl:for-each></main></xsl:template>"#,
            href.display()
        )),
        None,
    )
    .unwrap();
    let r = ss.transform(cameleer_xml::dom::Document::parse(ORDERS).unwrap(), &[]).unwrap();
    assert_eq!(r.output, "<main/>");
    assert_eq!(r.secondary.len(), 2);
    assert_eq!(std::fs::read_to_string(dir.join("out/o-3.xml")).unwrap(), "<o>5</o>");
    assert_eq!(std::fs::read_to_string(dir.join("out/o-2.xml")).unwrap(), "<o>7</o>");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn xpath2_in_xslt2_and_misc_instructions() {
    let out = run2(
        r#"<xsl:template match="/"><r><xsl:namespace name="p" select="'urn:p'"/><xsl:value-of select="for $o in //order[@total gt 4] return $o/@id"/>|<xsl:value-of select="distinct-values(//order/@customer)" separator=","/>|<xsl:value-of select="upper-case(string-join(tokenize('a,b', ','), ';'))"/>|<xsl:value-of select="system-property('xsl:version')"/>|<xsl:for-each select="reverse(1 to 3)"><xsl:value-of select="."/></xsl:for-each>|<xsl:perform-sort select="//order"><xsl:sort select="@total" data-type="number"/></xsl:perform-sort></r></xsl:template>
           <xsl:template match="order"><xsl:value-of select="@id"/></xsl:template>"#,
        ORDERS,
    );
    assert!(out.starts_with(r#"<r xmlns:p="urn:p">3 1 2|acme,globex,initech|A;B|2|321|"#), "{}", out);
}

#[test]
fn number_letter_value_and_lang() {
    let out = run1(
        r#"<xsl:template match="/"><xsl:number value="3" format="i" letter-value="alphabetic"/>,<xsl:number value="3" format="i"/>,<xsl:number value="2" format="a" lang="el"/>,<xsl:number value="3" format="A" lang="ru"/>,<xsl:number value="25" format="α" letter-value="traditional"/>,<xsl:number value="15" format="א"/>,<xsl:number value="28" format="a"/></xsl:template>"#,
        "<a/>",
    );
    assert_eq!(out, "k,iii,β,В,κεʹ,טו,ab");
}

#[test]
fn sort_collations_and_lang() {
    let xml = "<r><w>zeta</w><w>Émile</w><w>apple</w><w>éclair</w><w>Zoo</w><w>öl</w></r>";
    let out = run1(r#"<xsl:template match="/"><xsl:for-each select="r/w"><xsl:sort select="."/><xsl:value-of select="."/>,</xsl:for-each></xsl:template>"#, xml);
    assert_eq!(out, "apple,éclair,Émile,öl,zeta,Zoo,");
    let out = run1(r#"<xsl:template match="/"><xsl:for-each select="r/w"><xsl:sort select="." lang="sv"/><xsl:value-of select="."/>,</xsl:for-each></xsl:template>"#, xml);
    assert_eq!(out, "apple,éclair,Émile,zeta,Zoo,öl,");
    let out = run2(
        r#"<xsl:template match="/"><xsl:for-each select="r/w"><xsl:sort select="." collation="http://www.w3.org/2005/xpath-functions/collation/codepoint"/><xsl:value-of select="."/>,</xsl:for-each></xsl:template>"#,
        xml,
    );
    assert_eq!(out, "Zoo,apple,zeta,Émile,éclair,öl,");
    let e = Stylesheet::compile(&xsl2(r#"<xsl:template match="/"><xsl:for-each select="r/w"><xsl:sort select="." collation="urn:nope"/></xsl:for-each></xsl:template>"#), None)
        .unwrap()
        .transform_to_string(xml, &[])
        .unwrap_err();
    assert!(e.message.contains("Unknown collation"), "{}", e.message);
}

#[test]
fn html_output_adds_meta_content_type() {
    let ss = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output method="html" encoding="ISO-8859-1"/><xsl:template match="/"><html><head><meta http-equiv="Content-Type" content="x"/><title>t</title></head><body/></html></xsl:template></xsl:stylesheet>"#;
    let out = transform(ss, "<a/>").unwrap();
    assert_eq!(out, r#"<html><head><META http-equiv="Content-Type" content="text/html; charset=ISO-8859-1"><title>t</title></head><body></body></html>"#);
    let ss2 = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:output method="html" include-content-type="no"/><xsl:template match="/"><html><head/></html></xsl:template></xsl:stylesheet>"#;
    assert_eq!(transform(ss2, "<a/>").unwrap(), "<html><head></head></html>");
}
