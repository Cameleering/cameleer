//! An XML Schema 1.0 (XSD) validator for a practical subset.
//!
//! Schema components: global and local `element`s (`name`, `ref`, `type`, inline types,
//! `minOccurs`/`maxOccurs`, `nillable`, `fixed`, `form`), `complexType` (`sequence`,
//! `choice`, `all`, nested groups with occurrence constraints, `any` wildcards, `mixed`,
//! empty content, `simpleContent` extension / restriction, `complexContent` extension /
//! restriction, named model `group`s), `attribute` (`use="required"`, `type`, `fixed`,
//! `default`, `ref`, `form`), `attributeGroup`, `anyAttribute`, `simpleType` (`restriction`
//! with `enumeration`, `pattern`, `length`, `minLength`, `maxLength`, `minInclusive`,
//! `maxInclusive`, `minExclusive`, `maxExclusive`, `totalDigits`, `fractionDigits`,
//! `whiteSpace`; `list`; `union`), `include` and `import` (files relative to the schema),
//! `targetNamespace`, `elementFormDefault` / `attributeFormDefault`, `xsi:nil`.
//!
//! Built-in types: `string`, `normalizedString`, `token`, `language`, `Name`, `NCName`,
//! `NMTOKEN(S)`, `ID`, `IDREF(S)`, `ENTITY`, `QName`, `anyURI`, `boolean`, `decimal`,
//! `integer` and all its derived types (`int`, `long`, `short`, `byte`, `nonNegativeInteger`,
//! `positiveInteger`, `unsigned*`...), `float`, `double`, `date`, `dateTime`, `time`,
//! `duration`, `gYear`, `gYearMonth`, `gMonth`, `gDay`, `gMonthDay`, `base64Binary`,
//! `hexBinary`, `anyType`, `anySimpleType`.
//!
//! Not supported: identity constraints (`unique`/`key`/`keyref`), substitution groups,
//! `xsi:type`, `redefine`, block/final checks, ID/IDREF uniqueness.
//!
//! Errors use Xerces message codes (`cvc-complex-type.2.4.a: Invalid content was found
//! starting with element 'x'. One of '{a, b}' is expected.`) with line and column numbers.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use camel_core::regex::Regex;

use crate::dom::{Document, NodeId, NodeType};

/// The XML Schema namespace.
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
/// The XML Schema instance namespace.
pub const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

type Key = (String, String);

/// A validation error.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lineNumber: {}; columnNumber: {}; {}", self.line, self.col, self.message)
    }
}

#[derive(Clone, Debug, Default)]
struct Facets {
    enumeration: Vec<String>,
    patterns: Vec<(String, Regex)>,
    length: Option<usize>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    min_inclusive: Option<String>,
    max_inclusive: Option<String>,
    min_exclusive: Option<String>,
    max_exclusive: Option<String>,
    total_digits: Option<usize>,
    fraction_digits: Option<usize>,
    white_space: Option<String>,
}

#[derive(Clone, Debug)]
enum SimpleRef {
    Builtin(String),
    Named(Key),
    Inline(Box<SimpleType>),
}

#[derive(Clone, Debug)]
enum Variety {
    Atomic(SimpleRef),
    List(SimpleRef),
    Union(Vec<SimpleRef>),
}

#[derive(Clone, Debug)]
struct SimpleType {
    name: String,
    variety: Variety,
    facets: Facets,
}

#[derive(Clone, Debug)]
enum TypeRef {
    Named(Key),
    InlineSimple(Box<SimpleType>),
    InlineComplex(Box<ComplexType>),
    AnyType,
}

#[derive(Clone, Debug)]
struct ElementDecl {
    name: Key,
    ty: TypeRef,
    nillable: bool,
    fixed: Option<String>,
}

#[derive(Clone, Debug)]
enum Term {
    Element(Box<ElementDecl>),
    ElementRef(Key),
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
    All(Vec<Particle>),
    GroupRef(Key),
    Any { namespaces: Vec<String>, not: Option<String>, skip: bool },
}

#[derive(Clone, Debug)]
struct Particle {
    min: u32,
    max: Option<u32>,
    term: Term,
}

#[derive(Clone, Debug)]
struct AttrDecl {
    name: Key,
    ty: SimpleRef,
    required: bool,
    prohibited: bool,
    fixed: Option<String>,
}

#[derive(Clone, Debug)]
enum AttrItem {
    Attr(AttrDecl),
    Ref { name: Key, required: bool, prohibited: bool, fixed: Option<String> },
    Group(Key),
}

#[derive(Clone, Debug)]
enum Content {
    Empty,
    Particle(Particle),
    Simple(SimpleRef),
}

#[derive(Clone, Debug, PartialEq)]
enum Derivation {
    Extension,
    Restriction,
}

#[derive(Clone, Debug)]
struct ComplexType {
    mixed: bool,
    content: Content,
    attrs: Vec<AttrItem>,
    any_attribute: bool,
    base: Option<(Key, Derivation)>,
}

#[derive(Clone, Debug)]
enum TypeDef {
    Simple(SimpleType),
    Complex(ComplexType),
}

/// A compiled schema (possibly assembled from several documents).
#[derive(Debug, Default)]
pub struct Schema {
    elements: HashMap<Key, ElementDecl>,
    types: HashMap<Key, TypeDef>,
    groups: HashMap<Key, Particle>,
    attribute_groups: HashMap<Key, (Vec<AttrItem>, bool)>,
    attributes: HashMap<Key, AttrDecl>,
    loaded: Vec<PathBuf>,
}

fn serr(m: impl Into<String>) -> String {
    m.into()
}

struct Ctx<'a> {
    doc: &'a Document,
    tns: String,
    elem_qualified: bool,
    attr_qualified: bool,
    base: Option<PathBuf>,
}

fn is_xsd(doc: &Document, n: NodeId, local: &str) -> bool {
    doc.node_type(n) == NodeType::Element && doc.namespace_uri(n) == XSD_NS && doc.local_name(n) == local
}

fn xsd_children(doc: &Document, n: NodeId) -> Vec<NodeId> {
    doc.element_children(n).into_iter().filter(|&c| doc.namespace_uri(c) == XSD_NS && doc.local_name(c) != "annotation").collect()
}

impl Schema {
    /// Parse a schema from text; `base_dir` resolves `include` / `import` locations.
    pub fn parse(text: &str, base_dir: Option<&Path>) -> Result<Schema, String> {
        let doc = Document::parse(text).map_err(|e| format!("Invalid XML schema: {}", e))?;
        let mut s = Schema::default();
        s.load_doc(&doc, base_dir)?;
        Ok(s)
    }

    /// Parse a schema file.
    pub fn parse_file(path: &Path) -> Result<Schema, String> {
        let mut s = Schema::default();
        s.load_file(path)?;
        Ok(s)
    }

    fn load_file(&mut self, path: &Path) -> Result<(), String> {
        let canon = path.to_path_buf();
        if self.loaded.contains(&canon) {
            return Ok(());
        }
        self.loaded.push(canon);
        let bytes = std::fs::read(path).map_err(|e| format!("Cannot read schema {}: {}", path.display(), e))?;
        let doc = Document::parse_bytes(&bytes).map_err(|e| format!("Invalid XML schema {}: {}", path.display(), e))?;
        self.load_doc(&doc, path.parent())
    }

    fn load_doc(&mut self, doc: &Document, base: Option<&Path>) -> Result<(), String> {
        let root = doc.document_element().ok_or("Empty schema")?;
        if !is_xsd(doc, root, "schema") {
            return Err(format!("Not an XML schema: root element is <{}>", doc.qualified_name(root)));
        }
        let ctx = Ctx {
            doc,
            tns: doc.attribute(root, "targetNamespace").unwrap_or("").to_string(),
            elem_qualified: doc.attribute(root, "elementFormDefault") == Some("qualified"),
            attr_qualified: doc.attribute(root, "attributeFormDefault") == Some("qualified"),
            base: base.map(|b| b.to_path_buf()),
        };
        for c in xsd_children(doc, root) {
            let local = doc.local_name(c).to_string();
            match local.as_str() {
                "include" | "import" | "redefine" => {
                    if let Some(loc) = doc.attribute(c, "schemaLocation") {
                        let p = PathBuf::from(loc.strip_prefix("file:").unwrap_or(loc));
                        let p = if p.is_absolute() { p } else { ctx.base.as_ref().map(|b| b.join(&p)).unwrap_or(p) };
                        self.load_file(&p)?;
                    }
                }
                "element" => {
                    let d = self.element_decl(&ctx, c, true)?;
                    self.elements.insert(d.name.clone(), d);
                }
                "complexType" => {
                    let name = doc.attribute(c, "name").ok_or("A global complexType requires a name")?;
                    let ct = self.complex_type(&ctx, c)?;
                    self.types.insert((ctx.tns.clone(), name.to_string()), TypeDef::Complex(ct));
                }
                "simpleType" => {
                    let name = doc.attribute(c, "name").ok_or("A global simpleType requires a name")?;
                    let st = self.simple_type(&ctx, c)?;
                    self.types.insert((ctx.tns.clone(), name.to_string()), TypeDef::Simple(st));
                }
                "group" => {
                    let name = doc.attribute(c, "name").ok_or("A global group requires a name")?;
                    let inner = xsd_children(doc, c).into_iter().next().ok_or("Empty group")?;
                    let p = self.particle(&ctx, inner)?;
                    self.groups.insert((ctx.tns.clone(), name.to_string()), p);
                }
                "attributeGroup" => {
                    let name = doc.attribute(c, "name").ok_or("A global attributeGroup requires a name")?;
                    let (items, any) = self.attr_items(&ctx, c)?;
                    self.attribute_groups.insert((ctx.tns.clone(), name.to_string()), (items, any));
                }
                "attribute" => {
                    let a = self.attr_decl(&ctx, c, true)?;
                    self.attributes.insert(a.name.clone(), a);
                }
                "notation" => {}
                other => return Err(serr(format!("Unsupported top-level schema component xs:{}", other))),
            }
        }
        Ok(())
    }

    fn qname(ctx: &Ctx, el: NodeId, q: &str) -> Result<Key, String> {
        let q = q.trim();
        match q.split_once(':') {
            Some((p, l)) => match ctx.doc.lookup_namespace(el, p) {
                Some(u) => Ok((u, l.to_string())),
                None => Err(format!("Undeclared prefix '{}' in schema QName '{}'", p, q)),
            },
            None => Ok((ctx.doc.lookup_namespace(el, "").unwrap_or_default(), q.to_string())),
        }
    }

    fn simple_ref(ctx: &Ctx, el: NodeId, q: &str) -> Result<SimpleRef, String> {
        let k = Self::qname(ctx, el, q)?;
        Ok(if k.0 == XSD_NS { SimpleRef::Builtin(k.1) } else { SimpleRef::Named(k) })
    }

    fn occurs(ctx: &Ctx, el: NodeId) -> Result<(u32, Option<u32>), String> {
        let min = match ctx.doc.attribute(el, "minOccurs") {
            Some(v) => v.trim().parse().map_err(|_| format!("Invalid minOccurs '{}'", v))?,
            None => 1,
        };
        let max = match ctx.doc.attribute(el, "maxOccurs") {
            Some("unbounded") => None,
            Some(v) => Some(v.trim().parse().map_err(|_| format!("Invalid maxOccurs '{}'", v))?),
            None => Some(1),
        };
        Ok((min, max))
    }

    fn element_decl(&mut self, ctx: &Ctx, el: NodeId, global: bool) -> Result<ElementDecl, String> {
        let doc = ctx.doc;
        let name = doc.attribute(el, "name").ok_or("An element declaration requires a name or ref")?;
        let qualified = global || doc.attribute(el, "form").map(|f| f == "qualified").unwrap_or(ctx.elem_qualified);
        let ns = if qualified { ctx.tns.clone() } else { String::new() };
        let ty = if let Some(t) = doc.attribute(el, "type") {
            let k = Self::qname(ctx, el, t)?;
            if k.0 == XSD_NS {
                if k.1 == "anyType" {
                    TypeRef::AnyType
                } else {
                    TypeRef::InlineSimple(Box::new(SimpleType { name: k.1.clone(), variety: Variety::Atomic(SimpleRef::Builtin(k.1)), facets: Facets::default() }))
                }
            } else {
                TypeRef::Named(k)
            }
        } else {
            let mut t = TypeRef::AnyType;
            for c in xsd_children(doc, el) {
                if is_xsd(doc, c, "complexType") {
                    t = TypeRef::InlineComplex(Box::new(self.complex_type(ctx, c)?));
                } else if is_xsd(doc, c, "simpleType") {
                    t = TypeRef::InlineSimple(Box::new(self.simple_type(ctx, c)?));
                }
            }
            t
        };
        Ok(ElementDecl { name: (ns, name.to_string()), ty, nillable: doc.attribute(el, "nillable") == Some("true"), fixed: doc.attribute(el, "fixed").map(String::from) })
    }

    fn particle(&mut self, ctx: &Ctx, el: NodeId) -> Result<Particle, String> {
        let doc = ctx.doc;
        let (min, max) = Self::occurs(ctx, el)?;
        let term = match doc.local_name(el) {
            "element" => {
                if let Some(r) = doc.attribute(el, "ref") {
                    Term::ElementRef(Self::qname(ctx, el, r)?)
                } else {
                    Term::Element(Box::new(self.element_decl(ctx, el, false)?))
                }
            }
            "sequence" | "choice" | "all" => {
                let mut parts = Vec::new();
                for c in xsd_children(doc, el) {
                    parts.push(self.particle(ctx, c)?);
                }
                match doc.local_name(el) {
                    "sequence" => Term::Sequence(parts),
                    "choice" => Term::Choice(parts),
                    _ => Term::All(parts),
                }
            }
            "group" => Term::GroupRef(Self::qname(ctx, el, doc.attribute(el, "ref").ok_or("group reference requires ref")?)?),
            "any" => {
                let ns = doc.attribute(el, "namespace").unwrap_or("##any");
                let skip = matches!(doc.attribute(el, "processContents"), Some("skip") | Some("lax"));
                let (namespaces, not) = match ns {
                    "##any" => (Vec::new(), None),
                    "##other" => (Vec::new(), Some(ctx.tns.clone())),
                    list => (
                        list.split_whitespace()
                            .map(|t| match t {
                                "##targetNamespace" => ctx.tns.clone(),
                                "##local" => String::new(),
                                o => o.to_string(),
                            })
                            .collect(),
                        None,
                    ),
                };
                let namespaces = if ns == "##any" { vec!["##any".to_string()] } else { namespaces };
                Term::Any { namespaces, not, skip }
            }
            other => return Err(format!("Unsupported particle xs:{}", other)),
        };
        Ok(Particle { min, max, term })
    }

    fn attr_decl(&mut self, ctx: &Ctx, el: NodeId, global: bool) -> Result<AttrDecl, String> {
        let doc = ctx.doc;
        let name = doc.attribute(el, "name").ok_or("An attribute declaration requires a name or ref")?;
        let qualified = global || doc.attribute(el, "form").map(|f| f == "qualified").unwrap_or(ctx.attr_qualified);
        let ns = if qualified { ctx.tns.clone() } else { String::new() };
        let ty = if let Some(t) = doc.attribute(el, "type") {
            Self::simple_ref(ctx, el, t)?
        } else if let Some(st) = xsd_children(doc, el).into_iter().find(|&c| is_xsd(doc, c, "simpleType")) {
            SimpleRef::Inline(Box::new(self.simple_type(ctx, st)?))
        } else {
            SimpleRef::Builtin("anySimpleType".into())
        };
        let use_ = doc.attribute(el, "use").unwrap_or("optional");
        Ok(AttrDecl { name: (ns, name.to_string()), ty, required: use_ == "required", prohibited: use_ == "prohibited", fixed: doc.attribute(el, "fixed").map(String::from) })
    }

    fn attr_items(&mut self, ctx: &Ctx, el: NodeId) -> Result<(Vec<AttrItem>, bool), String> {
        let doc = ctx.doc;
        let mut items = Vec::new();
        let mut any = false;
        for c in xsd_children(doc, el) {
            match doc.local_name(c) {
                "attribute" => {
                    if let Some(r) = doc.attribute(c, "ref") {
                        let use_ = doc.attribute(c, "use").unwrap_or("optional");
                        items.push(AttrItem::Ref {
                            name: Self::qname(ctx, c, r)?,
                            required: use_ == "required",
                            prohibited: use_ == "prohibited",
                            fixed: doc.attribute(c, "fixed").map(String::from),
                        });
                    } else {
                        items.push(AttrItem::Attr(self.attr_decl(ctx, c, false)?));
                    }
                }
                "attributeGroup" => items.push(AttrItem::Group(Self::qname(ctx, c, doc.attribute(c, "ref").ok_or("attributeGroup reference requires ref")?)?)),
                "anyAttribute" => any = true,
                _ => {}
            }
        }
        Ok((items, any))
    }

    fn complex_type(&mut self, ctx: &Ctx, el: NodeId) -> Result<ComplexType, String> {
        let doc = ctx.doc;
        let name = doc.attribute(el, "name").unwrap_or("").to_string();
        let mut mixed = doc.attribute(el, "mixed") == Some("true");
        let mut content = Content::Empty;
        let mut base = None;
        let (mut attrs, mut any_attribute) = self.attr_items(ctx, el)?;
        for c in xsd_children(doc, el) {
            match doc.local_name(c) {
                "sequence" | "choice" | "all" | "group" => content = Content::Particle(self.particle(ctx, c)?),
                "simpleContent" | "complexContent" => {
                    let simple = doc.local_name(c) == "simpleContent";
                    if doc.attribute(c, "mixed") == Some("true") {
                        mixed = true;
                    }
                    let der = xsd_children(doc, c).into_iter().next().ok_or("Empty simpleContent/complexContent")?;
                    let derivation = if doc.local_name(der) == "extension" { Derivation::Extension } else { Derivation::Restriction };
                    let b = Self::qname(ctx, der, doc.attribute(der, "base").ok_or("Derivation requires a base")?)?;
                    let (a, any) = self.attr_items(ctx, der)?;
                    attrs.extend(a);
                    any_attribute |= any;
                    if simple {
                        let base_ref = if b.0 == XSD_NS { SimpleRef::Builtin(b.1.clone()) } else { SimpleRef::Named(b.clone()) };
                        if derivation == Derivation::Restriction {
                            let facets = self.facets(ctx, der)?;
                            content = Content::Simple(SimpleRef::Inline(Box::new(SimpleType { name: name.clone(), variety: Variety::Atomic(base_ref), facets })));
                        } else {
                            content = Content::Simple(base_ref);
                        }
                        if b.0 != XSD_NS {
                            base = Some((b, derivation));
                        }
                    } else {
                        for p in xsd_children(doc, der) {
                            if matches!(doc.local_name(p), "sequence" | "choice" | "all" | "group") {
                                content = Content::Particle(self.particle(ctx, p)?);
                            }
                        }
                        if !(b.0 == XSD_NS && b.1 == "anyType") {
                            base = Some((b, derivation));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(ComplexType { mixed, content, attrs, any_attribute, base })
    }

    fn facets(&mut self, ctx: &Ctx, el: NodeId) -> Result<Facets, String> {
        let doc = ctx.doc;
        let mut f = Facets::default();
        for c in xsd_children(doc, el) {
            let v = doc.attribute(c, "value").unwrap_or("").to_string();
            let num = |v: &str| v.trim().parse::<usize>().map_err(|_| format!("Invalid facet value '{}'", v));
            match doc.local_name(c) {
                "enumeration" => f.enumeration.push(v),
                "pattern" => {
                    let re = Regex::new(&format!("^(?:{})$", xsd_regex(&v))).map_err(|e| format!("Invalid pattern facet '{}': {}", v, e.message))?;
                    f.patterns.push((v, re));
                }
                "length" => f.length = Some(num(&v)?),
                "minLength" => f.min_length = Some(num(&v)?),
                "maxLength" => f.max_length = Some(num(&v)?),
                "minInclusive" => f.min_inclusive = Some(v),
                "maxInclusive" => f.max_inclusive = Some(v),
                "minExclusive" => f.min_exclusive = Some(v),
                "maxExclusive" => f.max_exclusive = Some(v),
                "totalDigits" => f.total_digits = Some(num(&v)?),
                "fractionDigits" => f.fraction_digits = Some(num(&v)?),
                "whiteSpace" => f.white_space = Some(v),
                "simpleType" | "attribute" | "attributeGroup" | "anyAttribute" | "sequence" | "choice" | "all" | "group" => {}
                other => return Err(format!("Unsupported facet xs:{}", other)),
            }
        }
        Ok(f)
    }

    fn simple_type(&mut self, ctx: &Ctx, el: NodeId) -> Result<SimpleType, String> {
        let doc = ctx.doc;
        let name = doc.attribute(el, "name").unwrap_or("").to_string();
        let def = xsd_children(doc, el).into_iter().next().ok_or("Empty simpleType")?;
        let (variety, facets) = match doc.local_name(def) {
            "restriction" => {
                let base = match doc.attribute(def, "base") {
                    Some(b) => Self::simple_ref(ctx, def, b)?,
                    None => {
                        let st = xsd_children(doc, def).into_iter().find(|&c| is_xsd(doc, c, "simpleType")).ok_or("restriction requires a base")?;
                        SimpleRef::Inline(Box::new(self.simple_type(ctx, st)?))
                    }
                };
                (Variety::Atomic(base), self.facets(ctx, def)?)
            }
            "list" => {
                let item = match doc.attribute(def, "itemType") {
                    Some(t) => Self::simple_ref(ctx, def, t)?,
                    None => {
                        let st = xsd_children(doc, def).into_iter().find(|&c| is_xsd(doc, c, "simpleType")).ok_or("list requires an itemType")?;
                        SimpleRef::Inline(Box::new(self.simple_type(ctx, st)?))
                    }
                };
                (Variety::List(item), Facets::default())
            }
            "union" => {
                let mut members = Vec::new();
                if let Some(m) = doc.attribute(def, "memberTypes") {
                    for t in m.split_whitespace() {
                        members.push(Self::simple_ref(ctx, def, t)?);
                    }
                }
                for st in xsd_children(doc, def) {
                    if is_xsd(doc, st, "simpleType") {
                        members.push(SimpleRef::Inline(Box::new(self.simple_type(ctx, st)?)));
                    }
                }
                (Variety::Union(members), Facets::default())
            }
            other => return Err(format!("Unsupported simpleType derivation xs:{}", other)),
        };
        Ok(SimpleType { name, variety, facets })
    }

    /// Validate a document; returns all errors found (empty when valid).
    pub fn validate(&self, doc: &Document) -> Vec<ValidationError> {
        let mut v = Validator { s: self, doc, errors: Vec::new() };
        match doc.document_element() {
            None => v.err(0, "Premature end of file.".into()),
            Some(root) => {
                let key = (doc.namespace_uri(root).to_string(), doc.local_name(root).to_string());
                match self.elements.get(&key) {
                    Some(d) => v.element(root, d),
                    None => v.err(root, format!("cvc-elt.1.a: Cannot find the declaration of element '{}'.", doc.qualified_name(root))),
                }
            }
        }
        v.errors
    }

    /// Global element names `(namespace, local)`.
    pub fn global_elements(&self) -> Vec<(String, String)> {
        let mut v: Vec<Key> = self.elements.keys().cloned().collect();
        v.sort();
        v
    }
}

/// Translate XML Schema regex syntax to the core regex syntax (`\i`, `\c` classes).
fn xsd_regex(p: &str) -> String {
    p.replace("\\i", "[_:A-Za-z]").replace("\\I", "[^_:A-Za-z]").replace("\\c", "[-._:A-Za-z0-9]").replace("\\C", "[^-._:A-Za-z0-9]")
}

// ---------------------------------------------------------------------------
// built-in types
// ---------------------------------------------------------------------------

fn collapse(s: &str) -> String {
    s.split([' ', '\t', '\n', '\r']).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

fn builtin_whitespace(name: &str) -> &'static str {
    match name {
        "string" | "anySimpleType" => "preserve",
        "normalizedString" => "replace",
        _ => "collapse",
    }
}

fn digits_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn is_decimal(s: &str) -> bool {
    let b = s.strip_prefix(['+', '-']).unwrap_or(s);
    match b.split_once('.') {
        Some((i, f)) => (i.is_empty() || digits_ok(i)) && (f.is_empty() || digits_ok(f)) && !(i.is_empty() && f.is_empty()),
        None => digits_ok(b),
    }
}

fn is_integer(s: &str) -> bool {
    digits_ok(s.strip_prefix(['+', '-']).unwrap_or(s))
}

fn is_double(s: &str) -> bool {
    if matches!(s, "INF" | "-INF" | "+INF" | "NaN") {
        return true;
    }
    let (m, e) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    is_decimal(m) && e.map(is_integer).unwrap_or(true)
}

fn tz_ok(s: &str) -> bool {
    if s.is_empty() || s == "Z" {
        return true;
    }
    let b = s.as_bytes();
    s.len() == 6 && (b[0] == b'+' || b[0] == b'-') && b[3] == b':' && digits_ok(&s[1..3]) && digits_ok(&s[4..6]) && s[1..3].parse::<u32>().unwrap() <= 14 && s[4..6].parse::<u32>().unwrap() < 60
}

fn split_tz(s: &str) -> (&str, &str) {
    if let Some(r) = s.strip_suffix('Z') {
        return (r, "Z");
    }
    if s.len() > 6 {
        let i = s.len() - 6;
        let c = s.as_bytes()[i];
        if (c == b'+' || c == b'-') && s.as_bytes()[i + 3] == b':' {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

fn date_ok(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or(s);
    let parts: Vec<&str> = b.splitn(3, '-').collect();
    if parts.len() != 3 || parts[0].len() < 4 || parts[1].len() != 2 || parts[2].len() != 2 || !parts.iter().all(|p| digits_ok(p)) {
        return false;
    }
    let y: i64 = parts[0].parse().unwrap_or(0);
    let m: u32 = parts[1].parse().unwrap_or(0);
    let d: u32 = parts[2].parse().unwrap_or(0);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    d >= 1 && d <= max
}

fn time_ok(s: &str) -> bool {
    let (hms, frac) = match s.split_once('.') {
        Some((a, f)) => (a, Some(f)),
        None => (s, None),
    };
    let p: Vec<&str> = hms.split(':').collect();
    if p.len() != 3 || !p.iter().all(|x| x.len() == 2 && digits_ok(x)) || frac.map(|f| !digits_ok(f)).unwrap_or(false) {
        return false;
    }
    let (h, m, sec): (u32, u32, u32) = (p[0].parse().unwrap(), p[1].parse().unwrap(), p[2].parse().unwrap());
    (h < 24 && m < 60 && sec < 60) || (h == 24 && m == 0 && sec == 0)
}

fn duration_ok(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or(s);
    let Some(rest) = b.strip_prefix('P') else { return false };
    if rest.is_empty() || rest.ends_with('T') {
        return false;
    }
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (rest, None),
    };
    let check = |part: &str, units: &[char]| -> bool {
        let mut num = String::new();
        let mut last = 0usize;
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else {
                let Some(i) = units.iter().position(|&u| u == c) else { return false };
                if num.is_empty() || i < last || (num.contains('.') && c != 'S') {
                    return false;
                }
                last = i + 1;
                num.clear();
            }
        }
        num.is_empty()
    };
    check(date, &['Y', 'M', 'D']) && time.map(|t| !t.is_empty() && check(t, &['H', 'M', 'S'])).unwrap_or(true)
}

/// Check a (whitespace-normalized) value against a built-in type.
fn builtin_valid(name: &str, v: &str) -> bool {
    let int_range = |lo: i128, hi: i128| is_integer(v) && v.parse::<i128>().map(|n| n >= lo && n <= hi).unwrap_or(false);
    match name {
        "string" | "anySimpleType" | "anyType" | "normalizedString" => true,
        "token" => !v.starts_with(' ') && !v.ends_with(' ') && !v.contains("  "),
        "language" => {
            let parts: Vec<&str> = v.split('-').collect();
            !v.is_empty() && parts[0].len() <= 8 && parts[0].chars().all(|c| c.is_ascii_alphabetic()) && parts.iter().all(|p| !p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_alphanumeric()))
        }
        "Name" => crate::parser::is_name(v),
        "NCName" | "ID" | "IDREF" | "ENTITY" => crate::parser::is_ncname(v),
        "IDREFS" | "ENTITIES" => !v.is_empty() && v.split(' ').all(crate::parser::is_ncname),
        "NMTOKEN" => !v.is_empty() && v.chars().all(crate::parser::is_name_char),
        "NMTOKENS" => !v.is_empty() && v.split(' ').all(|t| !t.is_empty() && t.chars().all(crate::parser::is_name_char)),
        "QName" | "NOTATION" => match v.split_once(':') {
            Some((p, l)) => crate::parser::is_ncname(p) && crate::parser::is_ncname(l),
            None => crate::parser::is_ncname(v),
        },
        "anyURI" => !v.contains(' ') || true,
        "boolean" => matches!(v, "true" | "false" | "1" | "0"),
        "decimal" => is_decimal(v),
        "float" | "double" => is_double(v),
        "integer" => is_integer(v),
        "long" => int_range(i64::MIN as i128, i64::MAX as i128),
        "int" => int_range(i32::MIN as i128, i32::MAX as i128),
        "short" => int_range(i16::MIN as i128, i16::MAX as i128),
        "byte" => int_range(i8::MIN as i128, i8::MAX as i128),
        "nonNegativeInteger" => is_integer(v) && !v.trim_start_matches('+').starts_with('-') || v.trim_start_matches('-').chars().all(|c| c == '0') && is_integer(v),
        "positiveInteger" => is_integer(v) && v.parse::<i128>().map(|n| n > 0).unwrap_or(!v.starts_with('-')),
        "nonPositiveInteger" => is_integer(v) && v.parse::<i128>().map(|n| n <= 0).unwrap_or(v.starts_with('-')),
        "negativeInteger" => is_integer(v) && v.parse::<i128>().map(|n| n < 0).unwrap_or(v.starts_with('-')),
        "unsignedLong" => int_range(0, u64::MAX as i128),
        "unsignedInt" => int_range(0, u32::MAX as i128),
        "unsignedShort" => int_range(0, u16::MAX as i128),
        "unsignedByte" => int_range(0, u8::MAX as i128),
        "date" => {
            let (d, tz) = split_tz(v);
            date_ok(d) && tz_ok(tz)
        }
        "dateTime" => {
            let (dt, tz) = split_tz(v);
            match dt.split_once('T') {
                Some((d, t)) => date_ok(d) && time_ok(t) && tz_ok(tz),
                None => false,
            }
        }
        "time" => {
            let (t, tz) = split_tz(v);
            time_ok(t) && tz_ok(tz)
        }
        "duration" => duration_ok(v),
        "gYear" => {
            let (y, tz) = split_tz(v);
            let y = y.strip_prefix('-').unwrap_or(y);
            y.len() >= 4 && digits_ok(y) && tz_ok(tz)
        }
        "gYearMonth" => {
            let (ym, tz) = split_tz(v);
            date_ok(&format!("{}-01", ym)) && tz_ok(tz)
        }
        "gMonth" => {
            let (m, tz) = split_tz(v);
            m.len() == 4 && m.starts_with("--") && m[2..].parse::<u32>().map(|x| (1..=12).contains(&x)).unwrap_or(false) && tz_ok(tz)
        }
        "gDay" => {
            let (d, tz) = split_tz(v);
            d.len() == 5 && d.starts_with("---") && d[3..].parse::<u32>().map(|x| (1..=31).contains(&x)).unwrap_or(false) && tz_ok(tz)
        }
        "gMonthDay" => {
            let (md, tz) = split_tz(v);
            md.len() == 7 && md.starts_with("--") && date_ok(&format!("2000-{}", &md[2..])) && tz_ok(tz)
        }
        "base64Binary" => {
            let c: String = v.chars().filter(|c| *c != ' ').collect();
            c.len() % 4 == 0 && c.trim_end_matches('=').chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/') && c.len() - c.trim_end_matches('=').len() <= 2
        }
        "hexBinary" => v.len() % 2 == 0 && v.chars().all(|c| c.is_ascii_hexdigit()),
        _ => true,
    }
}

fn is_numeric_builtin(name: &str) -> bool {
    matches!(
        name,
        "decimal" | "float" | "double" | "integer" | "long" | "int" | "short" | "byte" | "nonNegativeInteger" | "positiveInteger" | "nonPositiveInteger" | "negativeInteger" | "unsignedLong" | "unsignedInt" | "unsignedShort" | "unsignedByte"
    )
}

fn cmp_values(numeric: bool, a: &str, b: &str) -> Option<std::cmp::Ordering> {
    if numeric {
        let x: f64 = a.trim().trim_start_matches('+').parse().ok()?;
        let y: f64 = b.trim().trim_start_matches('+').parse().ok()?;
        x.partial_cmp(&y)
    } else {
        Some(a.cmp(b))
    }
}

// ---------------------------------------------------------------------------
// validation
// ---------------------------------------------------------------------------

struct Validator<'a> {
    s: &'a Schema,
    doc: &'a Document,
    errors: Vec<ValidationError>,
}

const MAX_ERRORS: usize = 100;

impl<'a> Validator<'a> {
    fn err(&mut self, node: NodeId, message: String) {
        if self.errors.len() < MAX_ERRORS {
            let n = self.doc.node(node);
            self.errors.push(ValidationError { message, line: n.line, col: n.col });
        }
    }

    fn builtin_root(&self, r: &SimpleRef, depth: usize) -> String {
        if depth > 50 {
            return "string".into();
        }
        match r {
            SimpleRef::Builtin(b) => b.clone(),
            SimpleRef::Named(k) => match self.s.types.get(k) {
                Some(TypeDef::Simple(st)) => self.st_root(st, depth + 1),
                Some(TypeDef::Complex(ct)) => match &ct.content {
                    Content::Simple(r) => self.builtin_root(r, depth + 1),
                    _ => "string".into(),
                },
                None => "string".into(),
            },
            SimpleRef::Inline(st) => self.st_root(st, depth + 1),
        }
    }

    fn st_root(&self, st: &SimpleType, depth: usize) -> String {
        match &st.variety {
            Variety::Atomic(b) => self.builtin_root(b, depth),
            _ => "string".into(),
        }
    }

    fn type_label(&self, r: &SimpleRef) -> String {
        match r {
            SimpleRef::Builtin(b) => b.clone(),
            SimpleRef::Named(k) => k.1.clone(),
            SimpleRef::Inline(st) => {
                if st.name.is_empty() {
                    "#AnonType".into()
                } else {
                    st.name.clone()
                }
            }
        }
    }

    /// Validate a value; returns an error message (Xerces code) on failure.
    fn simple_valid(&self, r: &SimpleRef, raw: &str, depth: usize) -> Result<(), String> {
        if depth > 50 {
            return Err("Simple type derivation is too deep or circular".into());
        }
        match r {
            SimpleRef::Builtin(b) => {
                let v = match builtin_whitespace(b) {
                    "preserve" => raw.to_string(),
                    "replace" => raw.replace(['\t', '\n', '\r'], " "),
                    _ => collapse(raw),
                };
                if builtin_valid(b, &v) {
                    Ok(())
                } else {
                    Err(format!("cvc-datatype-valid.1.2.1: '{}' is not a valid value for '{}'.", v, b))
                }
            }
            SimpleRef::Named(k) => match self.s.types.get(k) {
                Some(TypeDef::Simple(st)) => self.st_valid(st, raw, depth + 1),
                Some(TypeDef::Complex(ct)) => match &ct.content {
                    Content::Simple(r) => self.simple_valid(r, raw, depth + 1),
                    _ => Err(format!("Type '{}' is not a simple type", k.1)),
                },
                None => Err(format!("src-resolve: Cannot resolve the name '{}' to a(n) 'type definition' component.", k.1)),
            },
            SimpleRef::Inline(st) => self.st_valid(st, raw, depth + 1),
        }
    }

    fn st_valid(&self, st: &SimpleType, raw: &str, depth: usize) -> Result<(), String> {
        let label = if st.name.is_empty() { "#AnonType".to_string() } else { st.name.clone() };
        match &st.variety {
            Variety::Atomic(base) => {
                self.simple_valid(base, raw, depth)?;
                let root = self.builtin_root(base, 0);
                let ws = st.facets.white_space.clone().unwrap_or_else(|| builtin_whitespace(&root).to_string());
                let v = match ws.as_str() {
                    "preserve" => raw.to_string(),
                    "replace" => raw.replace(['\t', '\n', '\r'], " "),
                    _ => collapse(raw),
                };
                self.facets_valid(&st.facets, &v, v.chars().count(), is_numeric_builtin(&root), &label, &root)
            }
            Variety::List(item) => {
                let v = collapse(raw);
                let items: Vec<&str> = if v.is_empty() { Vec::new() } else { v.split(' ').collect() };
                for it in &items {
                    self.simple_valid(item, it, depth)?;
                }
                self.facets_valid(&st.facets, &v, items.len(), false, &label, "list")
            }
            Variety::Union(members) => {
                for m in members {
                    if self.simple_valid(m, raw, depth).is_ok() {
                        return self.facets_valid(&st.facets, &collapse(raw), collapse(raw).chars().count(), false, &label, "union");
                    }
                }
                Err(format!(
                    "cvc-datatype-valid.1.2.3: '{}' is not a valid value of union type '{}'.",
                    collapse(raw),
                    label
                ))
            }
        }
    }

    fn facets_valid(&self, f: &Facets, v: &str, len: usize, numeric: bool, label: &str, root: &str) -> Result<(), String> {
        if !f.enumeration.is_empty() {
            let ok = f.enumeration.iter().any(|e| {
                if numeric {
                    cmp_values(true, e, v) == Some(std::cmp::Ordering::Equal)
                } else {
                    e == v || collapse(e) == v
                }
            });
            if !ok {
                return Err(format!(
                    "cvc-enumeration-valid: Value '{}' is not facet-valid with respect to enumeration '[{}]'. It must be a value from the enumeration.",
                    v,
                    f.enumeration.join(", ")
                ));
            }
        }
        for (p, re) in &f.patterns {
            if !re.is_full_match(v) {
                return Err(format!("cvc-pattern-valid: Value '{}' is not facet-valid with respect to pattern '{}' for type '{}'.", v, p, label));
            }
        }
        // length facets do not apply to numbers / dates
        let lengthy = !is_numeric_builtin(root) && !matches!(root, "boolean" | "date" | "dateTime" | "time");
        let blen = if matches!(root, "hexBinary") { len / 2 } else { len };
        if lengthy {
            if let Some(l) = f.length {
                if blen != l {
                    return Err(format!("cvc-length-valid: Value '{}' with length = '{}' is not facet-valid with respect to length '{}' for type '{}'.", v, blen, l, label));
                }
            }
            if let Some(l) = f.min_length {
                if blen < l {
                    return Err(format!("cvc-minLength-valid: Value '{}' with length = '{}' is not facet-valid with respect to minLength '{}' for type '{}'.", v, blen, l, label));
                }
            }
            if let Some(l) = f.max_length {
                if blen > l {
                    return Err(format!("cvc-maxLength-valid: Value '{}' with length = '{}' is not facet-valid with respect to maxLength '{}' for type '{}'.", v, blen, l, label));
                }
            }
        }
        use std::cmp::Ordering::*;
        let checks: [(&Option<String>, &str, &[std::cmp::Ordering]); 4] = [
            (&f.min_inclusive, "minInclusive", &[Greater, Equal]),
            (&f.max_inclusive, "maxInclusive", &[Less, Equal]),
            (&f.min_exclusive, "minExclusive", &[Greater]),
            (&f.max_exclusive, "maxExclusive", &[Less]),
        ];
        for (bound, fname, ok) in checks {
            if let Some(b) = bound {
                match cmp_values(numeric, v, b) {
                    Some(o) if ok.contains(&o) => {}
                    _ => return Err(format!("cvc-{}-valid: Value '{}' is not facet-valid with respect to {} '{}' for type '{}'.", fname, v, fname, b, label)),
                }
            }
        }
        if numeric {
            let digits = v.trim_start_matches(['+', '-']);
            let (ip, fp) = digits.split_once('.').unwrap_or((digits, ""));
            let fp = fp.trim_end_matches('0');
            let ip = ip.trim_start_matches('0');
            if let Some(t) = f.total_digits {
                if ip.len() + fp.len() > t {
                    return Err(format!("cvc-totalDigits-valid: Value '{}' has {} total digits, but the number of total digits has been limited to {}.", v, ip.len() + fp.len(), t));
                }
            }
            if let Some(t) = f.fraction_digits {
                if fp.len() > t {
                    return Err(format!("cvc-fractionDigits-valid: Value '{}' has {} fraction digits, but the number of fraction digits has been limited to {}.", v, fp.len(), t));
                }
            }
        }
        Ok(())
    }

    fn resolve_type(&self, t: &'a TypeRef) -> Option<ResolvedType<'a>> {
        match t {
            TypeRef::AnyType => Some(ResolvedType::Any),
            TypeRef::InlineSimple(st) => Some(ResolvedType::Simple(SimpleRef::Inline(st.clone()))),
            TypeRef::InlineComplex(ct) => Some(ResolvedType::Complex(ct)),
            TypeRef::Named(k) => match self.s.types.get(k) {
                Some(TypeDef::Simple(_)) => Some(ResolvedType::Simple(SimpleRef::Named(k.clone()))),
                Some(TypeDef::Complex(ct)) => Some(ResolvedType::Complex(ct)),
                None => None,
            },
        }
    }

    /// Effective content and attribute items of a complex type (following derivations).
    fn effective(&self, ct: &'a ComplexType, depth: usize) -> (Content, Vec<AttrItem>, bool, bool) {
        let mut content = ct.content.clone();
        let mut attrs = Vec::new();
        let mut any = ct.any_attribute;
        let mut mixed = ct.mixed;
        if let Some((b, der)) = &ct.base {
            if depth < 30 {
                if let Some(TypeDef::Complex(bt)) = self.s.types.get(b) {
                    let (bc, ba, bany, bmixed) = self.effective(bt, depth + 1);
                    attrs = ba;
                    any |= bany;
                    mixed |= bmixed && *der == Derivation::Extension;
                    match der {
                        Derivation::Extension => {
                            content = match (bc, content) {
                                (Content::Empty, c) => c,
                                (c, Content::Empty) => c,
                                (Content::Particle(a), Content::Particle(b)) => {
                                    Content::Particle(Particle { min: 1, max: Some(1), term: Term::Sequence(vec![a, b]) })
                                }
                                (Content::Simple(s), _) => Content::Simple(s),
                                (_, c) => c,
                            };
                        }
                        Derivation::Restriction => {
                            if let (Content::Simple(_), Content::Empty) = (&bc, &content) {
                                content = bc;
                            }
                        }
                    }
                }
            }
        }
        // own attributes override inherited ones with the same name
        for a in &ct.attrs {
            if let AttrItem::Attr(d) = a {
                attrs.retain(|x| !matches!(x, AttrItem::Attr(y) if y.name == d.name));
            }
            attrs.push(a.clone());
        }
        (content, attrs, any, mixed)
    }

    fn flatten_attrs(&self, items: &[AttrItem], out: &mut Vec<AttrDecl>, any: &mut bool, depth: usize) {
        if depth > 30 {
            return;
        }
        for it in items {
            match it {
                AttrItem::Attr(d) => out.push(d.clone()),
                AttrItem::Ref { name, required, prohibited, fixed } => {
                    let mut d = self.s.attributes.get(name).cloned().unwrap_or(AttrDecl {
                        name: name.clone(),
                        ty: SimpleRef::Builtin("anySimpleType".into()),
                        required: false,
                        prohibited: false,
                        fixed: None,
                    });
                    d.required = *required;
                    d.prohibited = *prohibited;
                    if fixed.is_some() {
                        d.fixed = fixed.clone();
                    }
                    out.push(d);
                }
                AttrItem::Group(k) => {
                    if let Some((items, a)) = self.s.attribute_groups.get(k) {
                        *any |= *a;
                        self.flatten_attrs(items, out, any, depth + 1);
                    }
                }
            }
        }
    }

    fn element(&mut self, el: NodeId, decl: &ElementDecl) {
        let doc = self.doc;
        let qn = doc.qualified_name(el);
        let nil = doc.attribute_ns(el, XSI_NS, "nil").map(|v| v == "true" || v == "1").unwrap_or(false);
        if nil {
            if !decl.nillable {
                self.err(el, format!("cvc-elt.3.1: Attribute 'http://www.w3.org/2001/XMLSchema-instance,nil' must not appear on element '{}', because the {{nillable}} property of '{}' is false.", qn, qn));
            } else if !doc.children(el).iter().all(|&c| doc.node_type(c) == NodeType::Comment || doc.node_type(c) == NodeType::ProcessingInstruction) {
                self.err(el, format!("cvc-elt.3.2.1: Element '{}' cannot have character or element information [children], because 'http://www.w3.org/2001/XMLSchema-instance,nil' is specified.", qn));
            }
            return;
        }
        let Some(ty) = self.resolve_type(&decl.ty) else {
            if let TypeRef::Named(k) = &decl.ty {
                self.err(el, format!("src-resolve: Cannot resolve the name '{}' to a(n) 'type definition' component.", k.1));
            }
            return;
        };
        let text_children = |doc: &Document| -> String { doc.children(el).iter().filter(|&&c| doc.node_type(c) == NodeType::Text).map(|&c| doc.value(c)).collect() };
        match ty {
            ResolvedType::Any => {}
            ResolvedType::Simple(sref) => {
                if !doc.element_children(el).is_empty() {
                    self.err(el, format!("cvc-type.3.1.2: Element '{}' is a simple type, so it must have no element information item [children].", qn));
                    return;
                }
                for &a in doc.attributes(el) {
                    if doc.namespace_uri(a) != XSI_NS {
                        self.err(el, format!("cvc-type.3.1.1: Element '{}' is a simple type, so it cannot have attributes, excepting those whose namespace name is identical to 'http://www.w3.org/2001/XMLSchema-instance' and whose [local name] is one of 'type', 'nil', 'schemaLocation' or 'noNamespaceSchemaLocation'. However, the attribute, '{}' was found.", qn, doc.qualified_name(a)));
                    }
                }
                let text = text_children(doc);
                self.check_value(el, &sref, &text, &qn, decl.fixed.as_deref());
            }
            ResolvedType::Complex(ct) => {
                let (content, attr_items, mut any_attr, mixed) = self.effective(ct, 0);
                let mut attrs = Vec::new();
                self.flatten_attrs(&attr_items, &mut attrs, &mut any_attr, 0);
                self.attributes(el, &attrs, any_attr, &qn);
                match content {
                    Content::Empty => {
                        let has_text = !text_children(doc).trim().is_empty();
                        if !doc.element_children(el).is_empty() || (has_text && !mixed) {
                            self.err(el, format!("cvc-complex-type.2.1: Element '{}' must have no character or element information item [children], because the type's content type is empty.", qn));
                        }
                    }
                    Content::Simple(sref) => {
                        if !doc.element_children(el).is_empty() {
                            self.err(el, format!("cvc-complex-type.2.2: Element '{}' must have no element [children], and the value must be valid.", qn));
                            return;
                        }
                        let text = text_children(doc);
                        self.check_value(el, &sref, &text, &qn, decl.fixed.as_deref());
                    }
                    Content::Particle(p) => {
                        if !mixed {
                            if let Some(&t) = doc.children(el).iter().find(|&&c| doc.node_type(c) == NodeType::Text && !doc.value(c).trim().is_empty()) {
                                let _ = t;
                                self.err(el, format!("cvc-complex-type.2.3: Element '{}' cannot have character [children], because the type's content type is element-only.", qn));
                            }
                        }
                        self.content(el, &p, &qn);
                    }
                }
            }
        }
    }

    fn check_value(&mut self, el: NodeId, sref: &SimpleRef, text: &str, qn: &str, fixed: Option<&str>) {
        if let Err(m) = self.simple_valid(sref, text, 0) {
            self.err(el, m);
            self.err(el, format!("cvc-type.3.1.3: The value '{}' of element '{}' is not valid.", text.trim(), qn));
            return;
        }
        if let Some(f) = fixed {
            if collapse(text) != collapse(f) {
                self.err(el, format!("cvc-elt.5.2.2.2.2: The value '{}' of element '{}' does not match the {{value constraint}} value '{}'.", text, qn, f));
            }
        }
    }

    fn attributes(&mut self, el: NodeId, decls: &[AttrDecl], any: bool, qn: &str) {
        let doc = self.doc;
        for &a in doc.attributes(el) {
            let key = (doc.namespace_uri(a).to_string(), doc.local_name(a).to_string());
            if key.0 == XSI_NS || key.0 == crate::dom::XML_NS && any {
                continue;
            }
            match decls.iter().find(|d| d.name == key) {
                Some(d) if d.prohibited => self.err(el, format!("cvc-complex-type.3.2.2: Attribute '{}' is not allowed to appear in element '{}'.", doc.qualified_name(a), qn)),
                Some(d) => {
                    let v = doc.value(a).to_string();
                    if let Err(m) = self.simple_valid(&d.ty, &v, 0) {
                        self.err(el, m);
                        self.err(el, format!("cvc-attribute.3: The value '{}' of attribute '{}' on element '{}' is not valid with respect to its type, '{}'.", v, doc.qualified_name(a), qn, self.type_label(&d.ty)));
                    } else if let Some(f) = &d.fixed {
                        if collapse(&v) != collapse(f) {
                            self.err(el, format!("cvc-attribute.4: The value '{}' of attribute '{}' on element '{}' is not valid with respect to its fixed {{value constraint}}. The attribute must have a value of '{}'.", v, doc.qualified_name(a), qn, f));
                        }
                    }
                }
                None if any => {}
                None => self.err(el, format!("cvc-complex-type.3.2.2: Attribute '{}' is not allowed to appear in element '{}'.", doc.qualified_name(a), qn)),
            }
        }
        for d in decls {
            if d.required && doc.attribute_ns(el, &d.name.0, &d.name.1).is_none() {
                self.err(el, format!("cvc-complex-type.4: Attribute '{}' must appear on element '{}'.", d.name.1, qn));
            }
        }
    }

    fn content(&mut self, el: NodeId, p: &Particle, qn: &str) {
        let doc = self.doc;
        let kids = doc.element_children(el);
        let names: Vec<Key> = kids.iter().map(|&k| (doc.namespace_uri(k).to_string(), doc.local_name(k).to_string())).collect();
        let mut m = Matcher { v: self, names: &names, attempts: HashMap::new(), reach: 0, memo: HashMap::new() };
        let ends = m.particle(p, 0, 0);
        let complete = ends.contains(&names.len());
        let reach = m.reach;
        let attempts = std::mem::take(&mut m.attempts);
        if !complete {
            let expected = |pos: usize| -> String {
                let set: BTreeSet<String> = attempts.get(&pos).cloned().unwrap_or_default();
                let v: Vec<String> = set.into_iter().collect();
                v.join(", ")
            };
            if reach < names.len() {
                let bad = kids[reach];
                let exp = expected(reach);
                if exp.is_empty() {
                    self.err(bad, format!("cvc-complex-type.2.4.d: Invalid content was found starting with element '{}'. No child element is expected at this point.", doc.qualified_name(bad)));
                } else {
                    self.err(bad, format!("cvc-complex-type.2.4.a: Invalid content was found starting with element '{}'. One of '{{{}}}' is expected.", doc.qualified_name(bad), exp));
                }
            } else {
                self.err(el, format!("cvc-complex-type.2.4.b: The content of element '{}' is not complete. One of '{{{}}}' is expected.", qn, expected(names.len())));
            }
        }
        // validate children against their declarations
        let mut decls: Vec<(Key, Option<ElementDecl>, bool)> = Vec::new();
        self.collect_decls(p, &mut decls, 0);
        for (i, &k) in kids.iter().enumerate() {
            if !complete && i >= reach {
                // still validate children that have a declaration
            }
            match decls.iter().find(|(n, _, _)| *n == names[i]) {
                Some((_, Some(d), _)) => {
                    let d = d.clone();
                    self.element(k, &d);
                }
                Some((_, None, _)) => {}
                None => {
                    // wildcard: lax validation against global declarations
                    if let Some(d) = self.s.elements.get(&names[i]).cloned() {
                        if decls.iter().any(|(n, _, wild)| *wild && n.1 == "*") {
                            let skip = decls.iter().any(|(n, _, wild)| *wild && n.0 == "skip");
                            if !skip {
                                self.element(k, &d);
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_decls(&self, p: &Particle, out: &mut Vec<(Key, Option<ElementDecl>, bool)>, depth: usize) {
        if depth > 30 {
            return;
        }
        match &p.term {
            Term::Element(d) => out.push((d.name.clone(), Some((**d).clone()), false)),
            Term::ElementRef(k) => out.push((k.clone(), self.s.elements.get(k).cloned(), false)),
            Term::Sequence(ps) | Term::Choice(ps) | Term::All(ps) => {
                for q in ps {
                    self.collect_decls(q, out, depth + 1);
                }
            }
            Term::GroupRef(k) => {
                if let Some(g) = self.s.groups.get(k) {
                    self.collect_decls(g, out, depth + 1);
                }
            }
            Term::Any { skip, .. } => out.push(((if *skip { "skip".into() } else { "strict".into() }, "*".into()), None, true)),
        }
    }
}

enum ResolvedType<'a> {
    Any,
    Simple(SimpleRef),
    Complex(&'a ComplexType),
}

/// Content model matcher: computes the set of positions reachable after a particle.
struct Matcher<'v, 'a> {
    v: &'v Validator<'a>,
    names: &'v [Key],
    attempts: HashMap<usize, BTreeSet<String>>,
    reach: usize,
    memo: HashMap<(usize, usize), Vec<usize>>,
}

impl Matcher<'_, '_> {
    fn note(&mut self, pos: usize, name: String) {
        self.attempts.entry(pos).or_default().insert(name);
    }

    fn particle(&mut self, p: &Particle, start: usize, depth: usize) -> Vec<usize> {
        if depth > 60 {
            return Vec::new();
        }
        let key = (p as *const Particle as usize, start);
        if let Some(r) = self.memo.get(&key) {
            return r.clone();
        }
        let mut result: BTreeSet<usize> = BTreeSet::new();
        if p.min == 0 {
            result.insert(start);
        }
        let len = self.names.len() as u32;
        let limit = p.max.unwrap_or(u32::MAX).min(p.min.max(len + 1));
        let mut frontier: BTreeSet<usize> = BTreeSet::from([start]);
        let mut seen: BTreeSet<usize> = BTreeSet::from([start]);
        let mut count = 0u32;
        while count < limit && !frontier.is_empty() {
            count += 1;
            let mut next = BTreeSet::new();
            for &s in &frontier {
                next.extend(self.term(&p.term, s, depth));
            }
            if next.is_empty() {
                break;
            }
            if count >= p.min {
                result.extend(next.iter().copied());
                let fresh: BTreeSet<usize> = next.difference(&seen).copied().collect();
                if fresh.is_empty() {
                    break;
                }
                seen.extend(fresh.iter().copied());
                frontier = fresh;
            } else {
                frontier = next;
            }
        }
        let r: Vec<usize> = result.into_iter().collect();
        if let Some(&m) = r.iter().max() {
            self.reach = self.reach.max(m);
        }
        self.memo.insert(key, r.clone());
        r
    }

    fn element_at(&mut self, name: &Key, pos: usize) -> Vec<usize> {
        self.note(pos, if name.0.is_empty() { name.1.clone() } else { format!("\"{}\":{}", name.0, name.1) });
        if pos < self.names.len() && self.names[pos] == *name {
            self.reach = self.reach.max(pos + 1);
            vec![pos + 1]
        } else {
            Vec::new()
        }
    }

    fn term(&mut self, t: &Term, start: usize, depth: usize) -> Vec<usize> {
        match t {
            Term::Element(d) => self.element_at(&d.name, start),
            Term::ElementRef(k) => self.element_at(k, start),
            Term::Any { namespaces, not, .. } => {
                self.note(start, "WC[##any]".into());
                if start < self.names.len() {
                    let ns = &self.names[start].0;
                    let ok = match not {
                        Some(n) => ns != n && !ns.is_empty(),
                        None => namespaces.iter().any(|x| x == "##any" || x == ns),
                    };
                    if ok {
                        self.reach = self.reach.max(start + 1);
                        return vec![start + 1];
                    }
                }
                Vec::new()
            }
            Term::Sequence(ps) => {
                let mut cur: BTreeSet<usize> = BTreeSet::from([start]);
                for p in ps {
                    let mut next = BTreeSet::new();
                    for &s in &cur {
                        next.extend(self.particle(p, s, depth + 1));
                    }
                    if next.is_empty() {
                        return Vec::new();
                    }
                    cur = next;
                }
                cur.into_iter().collect()
            }
            Term::Choice(ps) => {
                let mut out = BTreeSet::new();
                for p in ps {
                    out.extend(self.particle(p, start, depth + 1));
                }
                out.into_iter().collect()
            }
            Term::All(ps) => {
                // each particle at most once (min 0 or 1), any order
                let mut out = BTreeSet::new();
                self.all(ps, start, 0u64, &mut out, depth);
                out.into_iter().collect()
            }
            Term::GroupRef(k) => {
                let schema = self.v.s;
                match schema.groups.get(k) {
                    Some(g) => self.particle(g, start, depth + 1),
                    None => Vec::new(),
                }
            }
        }
    }

    fn all(&mut self, ps: &[Particle], pos: usize, used: u64, out: &mut BTreeSet<usize>, depth: usize) {
        // complete when every required particle is used
        let complete = ps.iter().enumerate().all(|(i, p)| used & (1 << i) != 0 || p.min == 0);
        if complete {
            out.insert(pos);
        }
        for (i, p) in ps.iter().enumerate() {
            if used & (1 << i) != 0 || i >= 64 {
                continue;
            }
            let ends = self.term(&p.term, pos, depth + 1);
            for e in ends {
                if e > pos {
                    self.all(ps, e, used | (1 << i), out, depth);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins() {
        assert!(builtin_valid("int", "-42"));
        assert!(!builtin_valid("int", "4000000000"));
        assert!(builtin_valid("date", "2024-02-29"));
        assert!(!builtin_valid("date", "2023-02-29"));
        assert!(builtin_valid("dateTime", "2024-01-02T10:20:30.5Z"));
        assert!(builtin_valid("dateTime", "2024-01-02T10:20:30+02:00"));
        assert!(!builtin_valid("dateTime", "2024-01-02 10:20:30"));
        assert!(builtin_valid("decimal", "-1.50"));
        assert!(!builtin_valid("decimal", "1e3"));
        assert!(builtin_valid("double", "1e3"));
        assert!(builtin_valid("boolean", "1"));
        assert!(!builtin_valid("boolean", "yes"));
        assert!(builtin_valid("duration", "P1Y2M3DT4H5M6.5S"));
        assert!(!builtin_valid("duration", "P1S"));
        assert!(builtin_valid("positiveInteger", "7"));
        assert!(!builtin_valid("positiveInteger", "0"));
    }
}
