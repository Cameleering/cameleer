//! XML Schema validation.

use camel_xml::xsd::Schema;
use camel_xml::Document;

const ORDER_XSD: &str = r#"<?xml version="1.0"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:o="urn:orders" targetNamespace="urn:orders" elementFormDefault="qualified">
  <xs:element name="order" type="o:OrderType"/>
  <xs:complexType name="OrderType">
    <xs:sequence>
      <xs:element name="customer" type="o:Name"/>
      <xs:element name="date" type="xs:date"/>
      <xs:element name="created" type="xs:dateTime" minOccurs="0"/>
      <xs:choice>
        <xs:element name="email" type="o:Email"/>
        <xs:element name="phone" type="xs:string"/>
      </xs:choice>
      <xs:element name="item" type="o:ItemType" maxOccurs="3"/>
      <xs:element name="note" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
    </xs:sequence>
    <xs:attribute name="id" type="xs:int" use="required"/>
    <xs:attribute name="priority" type="o:Priority"/>
    <xs:attribute name="express" type="xs:boolean"/>
  </xs:complexType>
  <xs:complexType name="ItemType">
    <xs:all>
      <xs:element name="sku" type="o:Sku"/>
      <xs:element name="qty" type="o:Qty"/>
      <xs:element name="price" type="o:Price" minOccurs="0"/>
    </xs:all>
  </xs:complexType>
  <xs:simpleType name="Name"><xs:restriction base="xs:string"><xs:minLength value="2"/><xs:maxLength value="10"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Email"><xs:restriction base="xs:string"><xs:pattern value="[^@]+@[^@]+\.[a-z]+"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Priority"><xs:restriction base="xs:string"><xs:enumeration value="low"/><xs:enumeration value="high"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Sku"><xs:restriction base="xs:token"><xs:pattern value="[A-Z]{2}-\d{3}"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Qty"><xs:restriction base="xs:integer"><xs:minInclusive value="1"/><xs:maxInclusive value="100"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Price"><xs:restriction base="xs:decimal"><xs:minExclusive value="0"/><xs:fractionDigits value="2"/></xs:restriction></xs:simpleType>
</xs:schema>"#;

const VALID: &str = r#"<order xmlns="urn:orders" id="7" priority="high" express="true">
  <customer>Ann</customer>
  <date>2024-02-29</date>
  <email>ann@example.com</email>
  <item><qty>2</qty><sku>AB-123</sku><price>9.99</price></item>
  <item><sku> CD-456 </sku><qty>100</qty></item>
  <note>a</note><note>b</note>
</order>"#;

fn errors(xml: &str) -> Vec<String> {
    let s = Schema::parse(ORDER_XSD, None).unwrap();
    s.validate(&Document::parse(xml).unwrap()).into_iter().map(|e| e.message).collect()
}

fn first_error(xml: &str) -> String {
    let e = errors(xml);
    assert!(!e.is_empty(), "expected an error for {}", xml);
    e[0].clone()
}

#[test]
fn valid_document() {
    assert_eq!(errors(VALID), Vec::<String>::new());
}

#[test]
fn unknown_root_and_namespace() {
    assert_eq!(first_error("<order id='1'/>"), "cvc-elt.1.a: Cannot find the declaration of element 'order'.");
    assert!(first_error("<o:invoice xmlns:o='urn:orders'/>").contains("cvc-elt.1.a"));
}

#[test]
fn content_model_errors() {
    let e = first_error(&VALID.replace("<customer>Ann</customer>", ""));
    assert_eq!(e, "cvc-complex-type.2.4.a: Invalid content was found starting with element 'date'. One of '{\"urn:orders\":customer}' is expected.");
    let e = first_error(&VALID.replace("<email>ann@example.com</email>", ""));
    assert!(e.contains("Invalid content was found starting with element 'item'"), "{}", e);
    assert!(e.contains("email") && e.contains("phone"), "{}", e);
    let e = first_error(&VALID.replace("<note>a</note><note>b</note>", "").replace("<item><sku> CD-456 </sku><qty>100</qty></item>", "<item><sku>CD-456</sku><qty>1</qty></item><item><sku>CD-456</sku><qty>1</qty></item><item><sku>CD-456</sku><qty>1</qty></item>"));
    assert!(e.starts_with("cvc-complex-type.2.4.d") || e.starts_with("cvc-complex-type.2.4.a"), "{}", e);
    let incomplete = r#"<order xmlns="urn:orders" id="7"><customer>Ann</customer><date>2024-01-01</date><phone>1</phone></order>"#;
    assert_eq!(first_error(incomplete), "cvc-complex-type.2.4.b: The content of element 'order' is not complete. One of '{\"urn:orders\":item}' is expected.");
    let e = first_error(&VALID.replace("<price>9.99</price>", "<price>1</price><price>2</price>"));
    assert!(e.contains("price"), "{}", e);
    let e = first_error(&VALID.replace("<qty>100</qty>", ""));
    assert!(e.starts_with("cvc-complex-type.2.4.b"), "{}", e);
}

#[test]
fn text_in_element_only_content() {
    let e = first_error(&VALID.replace("<customer>Ann</customer>", "stray<customer>Ann</customer>"));
    assert!(e.starts_with("cvc-complex-type.2.3"), "{}", e);
}

#[test]
fn attribute_errors() {
    let e = errors(&VALID.replace(" id=\"7\"", ""));
    assert_eq!(e, vec!["cvc-complex-type.4: Attribute 'id' must appear on element 'order'.".to_string()]);
    let e = errors(&VALID.replace("id=\"7\"", "id=\"seven\""));
    assert_eq!(e[0], "cvc-datatype-valid.1.2.1: 'seven' is not a valid value for 'int'.");
    assert_eq!(e[1], "cvc-attribute.3: The value 'seven' of attribute 'id' on element 'order' is not valid with respect to its type, 'int'.");
    let e = first_error(&VALID.replace("priority=\"high\"", "priority=\"urgent\""));
    assert_eq!(e, "cvc-enumeration-valid: Value 'urgent' is not facet-valid with respect to enumeration '[low, high]'. It must be a value from the enumeration.");
    let e = first_error(&VALID.replace("express=\"true\"", "express=\"true\" extra=\"1\""));
    assert_eq!(e, "cvc-complex-type.3.2.2: Attribute 'extra' is not allowed to appear in element 'order'.");
    assert!(first_error(&VALID.replace("express=\"true\"", "express=\"yes\"")).contains("'yes' is not a valid value for 'boolean'"));
}

#[test]
fn simple_type_facets() {
    let e = first_error(&VALID.replace("<customer>Ann</customer>", "<customer>A</customer>"));
    assert_eq!(e, "cvc-minLength-valid: Value 'A' with length = '1' is not facet-valid with respect to minLength '2' for type 'Name'.");
    let e = first_error(&VALID.replace("<customer>Ann</customer>", "<customer>Annabelle-Lee</customer>"));
    assert!(e.starts_with("cvc-maxLength-valid"), "{}", e);
    let e = first_error(&VALID.replace("ann@example.com", "not-an-email"));
    assert_eq!(e, "cvc-pattern-valid: Value 'not-an-email' is not facet-valid with respect to pattern '[^@]+@[^@]+\\.[a-z]+' for type 'Email'.");
    let e = first_error(&VALID.replace("<qty>100</qty>", "<qty>101</qty>"));
    assert_eq!(e, "cvc-maxInclusive-valid: Value '101' is not facet-valid with respect to maxInclusive '100' for type 'Qty'.");
    let e = first_error(&VALID.replace("<qty>2</qty>", "<qty>0</qty>"));
    assert!(e.starts_with("cvc-minInclusive-valid"), "{}", e);
    let e = first_error(&VALID.replace("<price>9.99</price>", "<price>0</price>"));
    assert!(e.starts_with("cvc-minExclusive-valid"), "{}", e);
    let e = first_error(&VALID.replace("<price>9.99</price>", "<price>9.999</price>"));
    assert!(e.starts_with("cvc-fractionDigits-valid"), "{}", e);
    let e = first_error(&VALID.replace("2024-02-29", "2023-02-29"));
    assert_eq!(e, "cvc-datatype-valid.1.2.1: '2023-02-29' is not a valid value for 'date'.");
    let e = first_error(&VALID.replace("<email>", "<created>2024-01-01</created><email>"));
    assert!(e.contains("'2024-01-01' is not a valid value for 'dateTime'"), "{}", e);
    assert!(errors(&VALID.replace("<email>", "<created>2024-01-01T10:00:00Z</created><email>")).is_empty());
    let e = first_error(&VALID.replace("AB-123", "ab-123"));
    assert!(e.starts_with("cvc-pattern-valid"), "{}", e);
    let e = first_error(&VALID.replace("<qty>2</qty>", "<qty>two</qty>"));
    assert_eq!(e, "cvc-datatype-valid.1.2.1: 'two' is not a valid value for 'integer'.");
}

#[test]
fn errors_carry_line_numbers() {
    let s = Schema::parse(ORDER_XSD, None).unwrap();
    let errs = s.validate(&Document::parse(&VALID.replace("<qty>100</qty>", "<qty>101</qty>")).unwrap());
    assert_eq!(errs[0].line, 6);
    assert!(errs[0].to_string().starts_with("lineNumber: 6; columnNumber:"));
}

#[test]
fn no_namespace_schema_extensions_groups_and_lists() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="shapes">
        <xs:complexType><xs:sequence>
          <xs:element ref="circle" minOccurs="0" maxOccurs="unbounded"/>
          <xs:group ref="extras"/>
        </xs:sequence></xs:complexType>
      </xs:element>
      <xs:element name="circle" type="Circle"/>
      <xs:group name="extras"><xs:sequence><xs:element name="label" type="Label" minOccurs="0"/><xs:element name="sizes" type="IntList" minOccurs="0"/><xs:element name="color" type="Color" minOccurs="0"/></xs:sequence></xs:group>
      <xs:complexType name="Shape"><xs:sequence><xs:element name="x" type="xs:decimal"/></xs:sequence><xs:attribute name="id" type="xs:ID" use="required"/></xs:complexType>
      <xs:complexType name="Circle"><xs:complexContent><xs:extension base="Shape"><xs:sequence><xs:element name="r" type="xs:positiveInteger"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
      <xs:complexType name="Label"><xs:simpleContent><xs:extension base="xs:string"><xs:attribute name="lang" type="xs:language" use="required"/></xs:extension></xs:simpleContent></xs:complexType>
      <xs:simpleType name="IntList"><xs:list itemType="xs:int"/></xs:simpleType>
      <xs:simpleType name="Color"><xs:union memberTypes="xs:int"><xs:simpleType><xs:restriction base="xs:string"><xs:enumeration value="red"/></xs:restriction></xs:simpleType></xs:union></xs:simpleType>
    </xs:schema>"#;
    let s = Schema::parse(xsd, None).unwrap();
    let v = |x: &str| s.validate(&Document::parse(x).unwrap()).into_iter().map(|e| e.message).collect::<Vec<_>>();
    assert!(v("<shapes><circle id='c1'><x>1.5</x><r>2</r></circle><label lang='en'>hi</label><sizes>1 2 3</sizes><color>red</color></shapes>").is_empty());
    assert!(v("<shapes><color>5</color></shapes>").is_empty());
    assert!(v("<shapes><circle id='c1'><r>2</r></circle></shapes>")[0].contains("One of '{x}' is expected"));
    assert!(v("<shapes><circle id='c1'><x>1</x><r>0</r></circle></shapes>")[0].contains("positiveInteger"));
    assert!(v("<shapes><label>hi</label></shapes>")[0].contains("Attribute 'lang' must appear"));
    assert!(v("<shapes><sizes>1 two</sizes></shapes>")[0].contains("'two' is not a valid value for 'int'"));
    assert!(v("<shapes><color>blue</color></shapes>")[0].contains("union type"));
    assert!(v("<shapes><circle><x>1</x><r>1</r></circle></shapes>")[0].contains("Attribute 'id' must appear"));
}

#[test]
fn empty_mixed_any_and_nillable() {
    let xsd = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
      <xs:element name="r"><xs:complexType><xs:sequence>
        <xs:element name="empty" minOccurs="0"><xs:complexType/></xs:element>
        <xs:element name="para" minOccurs="0"><xs:complexType mixed="true"><xs:sequence><xs:element name="b" type="xs:string" minOccurs="0" maxOccurs="unbounded"/></xs:sequence></xs:complexType></xs:element>
        <xs:element name="ext" minOccurs="0"><xs:complexType><xs:sequence><xs:any processContents="skip" minOccurs="0" maxOccurs="unbounded"/></xs:sequence><xs:anyAttribute/></xs:complexType></xs:element>
        <xs:element name="opt" type="xs:int" nillable="true" minOccurs="0"/>
        <xs:element name="fixed" type="xs:string" fixed="v1" minOccurs="0"/>
      </xs:sequence></xs:complexType></xs:element>
    </xs:schema>"#;
    let s = Schema::parse(xsd, None).unwrap();
    let v = |x: &str| s.validate(&Document::parse(x).unwrap()).into_iter().map(|e| e.message).collect::<Vec<_>>();
    let xsi = "xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance'";
    assert!(v(&format!("<r {}><empty/><para>some <b>bold</b> text</para><ext any='1'><whatever><x/></whatever></ext><opt xsi:nil='true'/><fixed>v1</fixed></r>", xsi)).is_empty());
    assert!(v("<r><empty>x</empty></r>")[0].starts_with("cvc-complex-type.2.1"));
    assert!(v("<r><opt></opt></r>")[0].contains("not a valid value for 'int'"));
    assert!(v("<r><fixed>v2</fixed></r>")[0].contains("value constraint"));
}

#[test]
fn schema_load_errors() {
    assert!(Schema::parse("<notaschema/>", None).is_err());
    assert!(Schema::parse("<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema'><xs:simpleType name='p'><xs:restriction base='xs:string'><xs:pattern value='('/></xs:restriction></xs:simpleType></xs:schema>", None).is_err());
}
