//! A structural XML Schema model used to *type* XML ⇄ [`Value`] bindings (JAXB-like, used by
//! the `jaxb` data format and the cxf POJO mode).
//!
//! Validation itself is done by camel-xml's [`camel_xml::xsd::Schema`]; this model only keeps
//! what a binding needs: global elements, named / anonymous complex types (flattened
//! `sequence` / `choice` / `all` particles with their occurrence, `group` references,
//! `complexContent` extensions, `simpleContent`, attributes and `attributeGroup`s) and simple
//! types resolved to their built-in base type (`list` → arrays, `union` → string).
//!
//! Value mapping (see [`SchemaSet::element_to_value`] / [`SchemaSet::write_value`]):
//!
//! | XSD | Value |
//! |---|---|
//! | `int`, `long`, `integer`, `short`, `byte`, `unsigned*`, `*Integer` | `Int` |
//! | `decimal`, `float`, `double` | `Float` |
//! | `boolean` | `Bool` |
//! | `base64Binary`, `hexBinary` | `Bytes` |
//! | `date`, `dateTime`, `time`, `duration`, strings... | `String` (lexical form); with [`BindOptions::typed_dates`] `date`/`dateTime`/`time` are `XMLGregorianCalendar`-like objects (`year`, `month`, `day`, `hour`, `minute`, `second`, `millisecond`, `timezone` in minutes) and `duration` an object (`negative`, `years`, `months`, `days`, `hours`, `minutes`, `seconds`); marshal also accepts epoch milliseconds (`Int`) for `dateTime`/`date` |
//! | complex type | `Object` (child elements by local name, attributes as `@name` or `name`) |
//! | `maxOccurs > 1` | `Array` (always, even with one item) |
//! | simple content with attributes | `Object` with the text under the text key (`value`) |
//! | `xsi:nil="true"` | `Null` |
//! | `xsi:type="p:Derived"` | the derived type's mapping, plus `@xsi:type: "Derived"` (attribute prefix + `xsi:type`; marshal writes `xsi:type` and uses the named type) |
//! | substitution group members | keyed by the member's own name where the head element is declared (marshal accepts members in place of the head) |
//!
//! Without a declaration the mapping is structural: element-only content becomes an object,
//! repeated sibling names become arrays and text-only elements become strings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use camel_core::error::{CamelError, Result};
use camel_core::value::{ObjectMap, Value};
use camel_xml::dom::{Document, NodeId, NodeType, QName, XMLNS_NS};
use camel_xml::xsd::{XSD_NS, XSI_NS};

use crate::support::{clark, resolve_qname, Key};

/// An element declaration (global or local).
#[derive(Clone, Debug)]
pub struct ElementDecl {
    pub name: Key,
    pub ty: TypeRef,
    pub min: u32,
    /// `None` = unbounded.
    pub max: Option<u32>,
    pub nillable: bool,
    /// `ref=` to a global element (type taken from the global declaration).
    pub reference: bool,
}

impl ElementDecl {
    /// Whether the element may occur more than once.
    pub fn repeated(&self) -> bool {
        self.max != Some(1) && self.max != Some(0)
    }
}

/// A reference to a type.
#[derive(Clone, Debug)]
pub enum TypeRef {
    Named(Key),
    Simple(Box<SimpleDef>),
    Complex(Box<ComplexDef>),
    Any,
}

/// A simple type: built-in base, list item, or union (treated as string).
#[derive(Clone, Debug)]
pub struct SimpleDef {
    pub base: Key,
    pub list: bool,
    pub enumeration: Vec<String>,
}

#[derive(Clone, Debug)]
enum Item {
    Elem(ElementDecl),
    Group { name: Key, repeat: bool, optional: bool },
    Any,
}

#[derive(Clone, Debug)]
enum AttrItem {
    Attr(AttrDecl),
    Ref(Key, bool),
    Group(Key),
}

/// An attribute declaration.
#[derive(Clone, Debug)]
pub struct AttrDecl {
    pub name: Key,
    pub ty: TypeRef,
    pub required: bool,
}

/// A complex type.
#[derive(Clone, Debug, Default)]
pub struct ComplexDef {
    items: Vec<Item>,
    attrs: Vec<AttrItem>,
    simple_content: Option<TypeRef>,
    base: Option<Key>,
    pub mixed: bool,
}

#[derive(Clone, Debug)]
enum TypeDef {
    Simple(SimpleDef),
    Complex(ComplexDef),
}

/// The resolved shape of a type.
#[derive(Clone, Debug)]
pub enum TypeInfo {
    /// Built-in local name (`int`, `string`...) and whether it is a list.
    Simple { builtin: String, list: bool },
    Complex { children: Vec<ElementDecl>, attrs: Vec<AttrDecl>, simple: Option<(String, bool)>, any: bool },
    Unknown,
}

/// Options of the XML ⇄ Value mapping.
#[derive(Clone, Debug)]
pub struct BindOptions {
    /// Prefix of attribute keys (`@` by default; empty = plain keys).
    pub attr_prefix: String,
    /// Key holding the text of simple-content / mixed elements.
    pub text_key: String,
    /// Preferred prefixes `(uri, prefix)` when writing.
    pub prefixes: Vec<(String, String)>,
    /// Map `date` / `dateTime` / `time` / `duration` to objects instead of lexical strings.
    pub typed_dates: bool,
}

impl Default for BindOptions {
    fn default() -> Self {
        BindOptions { attr_prefix: "@".into(), text_key: "value".into(), prefixes: Vec::new(), typed_dates: false }
    }
}

impl BindOptions {
    /// The key holding an `xsi:type` (attribute prefix + `xsi:type`).
    pub fn type_key(&self) -> String {
        format!("{}xsi:type", self.attr_prefix)
    }

    fn typed(&self, text: &str, builtin: &str, list: bool) -> Value {
        if self.typed_dates && !list {
            if let Some(v) = temporal_value(text.trim(), builtin) {
                return v;
            }
        }
        typed_text(text, builtin, list)
    }
}

/// A set of schema documents.
#[derive(Clone, Debug, Default)]
pub struct SchemaSet {
    elements: HashMap<Key, ElementDecl>,
    types: HashMap<Key, TypeDef>,
    groups: HashMap<Key, Vec<Item>>,
    attr_groups: HashMap<Key, Vec<AttrItem>>,
    attributes: HashMap<Key, AttrDecl>,
    /// Substitution group head → direct members.
    substitutions: HashMap<Key, Vec<Key>>,
    loaded: Vec<PathBuf>,
    /// Target namespaces of the loaded schemas.
    pub target_namespaces: Vec<String>,
}

struct Ctx {
    tns: String,
    elem_q: bool,
    attr_q: bool,
}

fn xs(doc: &Document, n: NodeId) -> Vec<NodeId> {
    doc.element_children(n).into_iter().filter(|&c| doc.namespace_uri(c) == XSD_NS && doc.local_name(c) != "annotation").collect()
}

fn occurs(doc: &Document, n: NodeId) -> (u32, Option<u32>) {
    let min = doc.attribute(n, "minOccurs").and_then(|v| v.trim().parse().ok()).unwrap_or(1);
    let max = match doc.attribute(n, "maxOccurs").map(str::trim) {
        Some("unbounded") => None,
        Some(v) => Some(v.parse().unwrap_or(1)),
        None => Some(1),
    };
    (min, max)
}

impl SchemaSet {
    /// An empty set.
    pub fn new() -> SchemaSet {
        SchemaSet::default()
    }

    /// Parse a schema text (`base_dir` resolves `include` / `import` locations).
    pub fn parse(text: &str, base_dir: Option<&Path>) -> Result<SchemaSet> {
        let doc = Document::parse(text).map_err(|e| CamelError::illegal_argument(format!("Invalid XML schema: {}", e)))?;
        let mut s = SchemaSet::new();
        let root = doc.document_element().ok_or_else(|| CamelError::illegal_argument("Empty schema"))?;
        s.add_schema(&doc, root, base_dir)?;
        Ok(s)
    }

    /// Parse a schema file.
    pub fn parse_file(path: &Path) -> Result<SchemaSet> {
        let mut s = SchemaSet::new();
        s.load_file(path)?;
        Ok(s)
    }

    fn load_file(&mut self, path: &Path) -> Result<()> {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if self.loaded.contains(&canon) {
            return Ok(());
        }
        self.loaded.push(canon);
        let text = std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read schema {}: {}", path.display(), e)))?;
        let doc = Document::parse(&text).map_err(|e| CamelError::illegal_argument(format!("Invalid XML schema {}: {}", path.display(), e)))?;
        let root = doc.document_element().ok_or_else(|| CamelError::illegal_argument("Empty schema"))?;
        self.add_schema(&doc, root, path.parent())
    }

    /// Add an `xs:schema` element (e.g. from WSDL `types`).
    pub fn add_schema(&mut self, doc: &Document, schema: NodeId, base_dir: Option<&Path>) -> Result<()> {
        if doc.namespace_uri(schema) != XSD_NS || doc.local_name(schema) != "schema" {
            return Err(CamelError::illegal_argument(format!("Not an XML schema: <{}>", doc.qualified_name(schema))));
        }
        let c = Ctx {
            tns: doc.attribute(schema, "targetNamespace").unwrap_or("").to_string(),
            elem_q: doc.attribute(schema, "elementFormDefault") == Some("qualified"),
            attr_q: doc.attribute(schema, "attributeFormDefault") == Some("qualified"),
        };
        if !self.target_namespaces.contains(&c.tns) {
            self.target_namespaces.push(c.tns.clone());
        }
        for n in xs(doc, schema) {
            let name = doc.attribute(n, "name").unwrap_or("").to_string();
            let key = (c.tns.clone(), name.clone());
            match doc.local_name(n) {
                "include" | "import" | "redefine" => {
                    if let (Some(loc), Some(dir)) = (doc.attribute(n, "schemaLocation"), base_dir) {
                        if !loc.starts_with("http:") && !loc.starts_with("https:") {
                            let p = dir.join(loc);
                            if p.exists() {
                                self.load_file(&p)?;
                            }
                        }
                    }
                }
                "element" => {
                    let mut d = self.element(doc, n, &c, true);
                    d.min = 1;
                    d.max = Some(1);
                    if let Some(head) = doc.attribute(n, "substitutionGroup") {
                        // XSD 1.1 allows a list of heads
                        for h in head.split_whitespace() {
                            let hk = resolve_qname(doc, n, h);
                            let members = self.substitutions.entry(hk).or_default();
                            if !members.contains(&key) {
                                members.push(key.clone());
                            }
                        }
                        // a member without a type takes the head's type
                        if doc.attribute(n, "type").is_none() && !xs(doc, n).iter().any(|&x| matches!(doc.local_name(x), "complexType" | "simpleType")) {
                            if let Some(h) = head.split_whitespace().next() {
                                d.ty = TypeRef::Named((String::from("\u{0}head"), clark(&resolve_qname(doc, n, h))));
                            }
                        }
                    }
                    self.elements.insert(key, d);
                }
                "complexType" => {
                    let ct = self.complex(doc, n, &c);
                    self.types.insert(key, TypeDef::Complex(ct));
                }
                "simpleType" => {
                    let st = self.simple(doc, n);
                    self.types.insert(key, TypeDef::Simple(st));
                }
                "group" => {
                    let mut items = Vec::new();
                    for g in xs(doc, n) {
                        self.particle(doc, g, &c, false, false, &mut items);
                    }
                    self.groups.insert(key, items);
                }
                "attributeGroup" => {
                    let attrs = self.attr_items(doc, n, &c);
                    self.attr_groups.insert(key, attrs);
                }
                "attribute" => {
                    let ty = self.attr_type(doc, n);
                    self.attributes.insert(key.clone(), AttrDecl { name: key, ty, required: false });
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn element(&mut self, doc: &Document, n: NodeId, c: &Ctx, global: bool) -> ElementDecl {
        let (min, max) = occurs(doc, n);
        let nillable = doc.attribute(n, "nillable") == Some("true");
        if let Some(r) = doc.attribute(n, "ref") {
            return ElementDecl { name: resolve_qname(doc, n, r), ty: TypeRef::Any, min, max, nillable, reference: true };
        }
        let local = doc.attribute(n, "name").unwrap_or("").to_string();
        let qualified = global || doc.attribute(n, "form").map(|f| f == "qualified").unwrap_or(c.elem_q);
        let name = (if qualified { c.tns.clone() } else { String::new() }, local);
        let ty = if let Some(t) = doc.attribute(n, "type") {
            TypeRef::Named(resolve_qname(doc, n, t))
        } else {
            let mut ty = TypeRef::Named((XSD_NS.to_string(), "anyType".into()));
            for ch in xs(doc, n) {
                match doc.local_name(ch) {
                    "complexType" => ty = TypeRef::Complex(Box::new(self.complex(doc, ch, c))),
                    "simpleType" => ty = TypeRef::Simple(Box::new(self.simple(doc, ch))),
                    _ => {}
                }
            }
            ty
        };
        ElementDecl { name, ty, min, max, nillable, reference: false }
    }

    fn particle(&mut self, doc: &Document, n: NodeId, c: &Ctx, repeat: bool, optional: bool, out: &mut Vec<Item>) {
        let (min, max) = occurs(doc, n);
        match doc.local_name(n) {
            "element" => {
                let mut d = self.element(doc, n, c, false);
                if optional {
                    d.min = 0;
                }
                if repeat {
                    d.max = None;
                }
                out.push(Item::Elem(d));
            }
            "sequence" | "choice" | "all" => {
                let rep = repeat || max != Some(1);
                let opt = optional || min == 0 || doc.local_name(n) == "choice";
                for ch in xs(doc, n) {
                    self.particle(doc, ch, c, rep, opt, out);
                }
            }
            "group" => {
                if let Some(r) = doc.attribute(n, "ref") {
                    out.push(Item::Group { name: resolve_qname(doc, n, r), repeat: repeat || max != Some(1), optional: optional || min == 0 });
                }
            }
            "any" => out.push(Item::Any),
            _ => {}
        }
    }

    fn attr_type(&mut self, doc: &Document, n: NodeId) -> TypeRef {
        if let Some(t) = doc.attribute(n, "type") {
            return TypeRef::Named(resolve_qname(doc, n, t));
        }
        for ch in xs(doc, n) {
            if doc.local_name(ch) == "simpleType" {
                return TypeRef::Simple(Box::new(self.simple(doc, ch)));
            }
        }
        TypeRef::Named((XSD_NS.to_string(), "string".into()))
    }

    fn attr_items(&mut self, doc: &Document, n: NodeId, c: &Ctx) -> Vec<AttrItem> {
        let mut out = Vec::new();
        for a in xs(doc, n) {
            match doc.local_name(a) {
                "attribute" => {
                    let required = doc.attribute(a, "use") == Some("required");
                    if let Some(r) = doc.attribute(a, "ref") {
                        out.push(AttrItem::Ref(resolve_qname(doc, a, r), required));
                    } else {
                        let local = doc.attribute(a, "name").unwrap_or("").to_string();
                        let q = doc.attribute(a, "form").map(|f| f == "qualified").unwrap_or(c.attr_q);
                        let ty = self.attr_type(doc, a);
                        out.push(AttrItem::Attr(AttrDecl { name: (if q { c.tns.clone() } else { String::new() }, local), ty, required }));
                    }
                }
                "attributeGroup" => {
                    if let Some(r) = doc.attribute(a, "ref") {
                        out.push(AttrItem::Group(resolve_qname(doc, a, r)));
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn complex(&mut self, doc: &Document, n: NodeId, c: &Ctx) -> ComplexDef {
        let mut ct = ComplexDef { mixed: doc.attribute(n, "mixed") == Some("true"), ..Default::default() };
        for ch in xs(doc, n) {
            match doc.local_name(ch) {
                "sequence" | "choice" | "all" | "group" => {
                    let mut items = Vec::new();
                    self.particle(doc, ch, c, false, false, &mut items);
                    ct.items.extend(items);
                }
                "attribute" | "attributeGroup" => {}
                "anyAttribute" => {}
                "simpleContent" | "complexContent" => {
                    let simple = doc.local_name(ch) == "simpleContent";
                    if doc.attribute(ch, "mixed") == Some("true") {
                        ct.mixed = true;
                    }
                    for d in xs(doc, ch) {
                        let base = doc.attribute(d, "base").map(|b| resolve_qname(doc, d, b));
                        if simple {
                            ct.simple_content = base.clone().map(TypeRef::Named);
                            if doc.local_name(d) == "restriction" {
                                if let Some(st) = xs(doc, d).into_iter().find(|&x| doc.local_name(x) == "simpleType") {
                                    ct.simple_content = Some(TypeRef::Simple(Box::new(self.simple(doc, st))));
                                }
                            }
                        } else if doc.local_name(d) == "extension" {
                            ct.base = base.clone();
                        }
                        if simple && doc.local_name(d) == "extension" {
                            ct.base = base.clone().filter(|b| b.0 != XSD_NS);
                        }
                        for p in xs(doc, d) {
                            if matches!(doc.local_name(p), "sequence" | "choice" | "all" | "group") {
                                let mut items = Vec::new();
                                self.particle(doc, p, c, false, false, &mut items);
                                ct.items.extend(items);
                            }
                        }
                        ct.attrs.extend(self.attr_items(doc, d, c));
                    }
                }
                _ => {}
            }
        }
        ct.attrs.extend(self.attr_items(doc, n, c));
        ct
    }

    fn simple(&mut self, doc: &Document, n: NodeId) -> SimpleDef {
        for d in xs(doc, n) {
            match doc.local_name(d) {
                "restriction" => {
                    let enumeration = xs(doc, d).into_iter().filter(|&f| doc.local_name(f) == "enumeration").filter_map(|f| doc.attribute(f, "value").map(str::to_string)).collect();
                    if let Some(b) = doc.attribute(d, "base") {
                        return SimpleDef { base: resolve_qname(doc, d, b), list: false, enumeration };
                    }
                    if let Some(inner) = xs(doc, d).into_iter().find(|&x| doc.local_name(x) == "simpleType") {
                        let mut s = self.simple(doc, inner);
                        s.enumeration = enumeration;
                        return s;
                    }
                }
                "list" => {
                    if let Some(t) = doc.attribute(d, "itemType") {
                        return SimpleDef { base: resolve_qname(doc, d, t), list: true, enumeration: Vec::new() };
                    }
                    if let Some(inner) = xs(doc, d).into_iter().find(|&x| doc.local_name(x) == "simpleType") {
                        let s = self.simple(doc, inner);
                        return SimpleDef { base: s.base, list: true, enumeration: Vec::new() };
                    }
                }
                _ => {}
            }
        }
        SimpleDef { base: (XSD_NS.to_string(), "string".into()), list: false, enumeration: Vec::new() }
    }

    // ------------------------------------------------------------------ queries

    /// A global element declaration.
    pub fn global_element(&self, ns: &str, local: &str) -> Option<&ElementDecl> {
        self.elements.get(&(ns.to_string(), local.to_string()))
    }

    /// Merge another schema set into this one.
    pub fn merge(&mut self, other: &SchemaSet) {
        self.elements.extend(other.elements.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.types.extend(other.types.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.groups.extend(other.groups.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.attr_groups.extend(other.attr_groups.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.attributes.extend(other.attributes.iter().map(|(k, v)| (k.clone(), v.clone())));
        for (h, ms) in &other.substitutions {
            let e = self.substitutions.entry(h.clone()).or_default();
            for m in ms {
                if !e.contains(m) {
                    e.push(m.clone());
                }
            }
        }
        for ns in &other.target_namespaces {
            if !self.target_namespaces.contains(ns) {
                self.target_namespaces.push(ns.clone());
            }
        }
    }

    /// All (transitive) members of the substitution group headed by `head`.
    pub fn substitution_members(&self, head: &Key) -> Vec<Key> {
        let mut out: Vec<Key> = Vec::new();
        let mut todo = vec![head.clone()];
        while let Some(h) = todo.pop() {
            for m in self.substitutions.get(&h).into_iter().flatten() {
                if !out.contains(m) && m != head {
                    out.push(m.clone());
                    todo.push(m.clone());
                }
            }
        }
        out
    }

    /// Find a named type by `local`, `{uri}local` or `prefix:local` (prefix bound in
    /// `prefixes` `(uri, prefix)` or matching a target namespace's preferred prefix).
    pub fn find_type(&self, name: &str, prefixes: &[(String, String)]) -> Option<Key> {
        let k = crate::support::parse_clark(name);
        if !k.0.is_empty() {
            return (self.types.contains_key(&k) || k.0 == XSD_NS).then_some(k);
        }
        if let Some((p, local)) = name.split_once(':') {
            if let Some((u, _)) = prefixes.iter().find(|(_, x)| x == p) {
                let k = (u.clone(), local.to_string());
                return self.types.contains_key(&k).then_some(k);
            }
            if p == "xs" || p == "xsd" {
                return Some((XSD_NS.to_string(), local.to_string()));
            }
            return self.find_type(local, prefixes);
        }
        let mut hits: Vec<&Key> = self.types.keys().filter(|k| k.1 == name).collect();
        hits.sort();
        hits.first().map(|k| (*k).clone())
    }

    /// The name to record for a type in `@xsi:type`: its local name when unambiguous, else `{uri}local`.
    fn type_label(&self, k: &Key) -> String {
        if self.types.keys().filter(|x| x.1 == k.1).count() <= 1 {
            k.1.clone()
        } else {
            clark(k)
        }
    }

    /// The shape of a named type.
    pub fn named_type_info(&self, k: &Key) -> TypeInfo {
        self.info_of_key(k, 0)
    }

    /// Global element names, sorted.
    pub fn global_elements(&self) -> Vec<Key> {
        let mut v: Vec<Key> = self.elements.keys().cloned().collect();
        v.sort();
        v
    }

    /// Resolve an element reference to the global declaration (keeping the occurrence).
    pub fn resolve_element(&self, d: &ElementDecl) -> ElementDecl {
        if d.reference {
            if let Some(g) = self.elements.get(&d.name) {
                return ElementDecl { min: d.min, max: d.max, reference: false, ..g.clone() };
            }
        }
        d.clone()
    }

    fn builtin_of(&self, k: &Key, depth: usize) -> (String, bool) {
        if k.0 == XSD_NS || depth > 32 {
            return (k.1.clone(), false);
        }
        match self.types.get(k) {
            Some(TypeDef::Simple(s)) => self.simple_builtin(s, depth + 1),
            Some(TypeDef::Complex(c)) => match &c.simple_content {
                Some(t) => self.ref_builtin(t, depth + 1),
                None => ("string".into(), false),
            },
            None => ("string".into(), false),
        }
    }

    fn simple_builtin(&self, s: &SimpleDef, depth: usize) -> (String, bool) {
        let (b, l) = self.builtin_of(&s.base, depth + 1);
        (b, l || s.list)
    }

    fn ref_builtin(&self, t: &TypeRef, depth: usize) -> (String, bool) {
        match t {
            TypeRef::Named(k) => self.builtin_of(k, depth),
            TypeRef::Simple(s) => self.simple_builtin(s, depth),
            _ => ("string".into(), false),
        }
    }

    fn expand_items(&self, items: &[Item], repeat: bool, optional: bool, out: &mut Vec<ElementDecl>, any: &mut bool, depth: usize) {
        if depth > 32 {
            return;
        }
        for it in items {
            match it {
                Item::Elem(d) => {
                    let mut d = d.clone();
                    if repeat {
                        d.max = None;
                    }
                    if optional {
                        d.min = 0;
                    }
                    out.push(d);
                }
                Item::Group { name, repeat: r, optional: o } => {
                    if let Some(g) = self.groups.get(name) {
                        self.expand_items(g, repeat || *r, optional || *o, out, any, depth + 1);
                    }
                }
                Item::Any => *any = true,
            }
        }
    }

    fn expand_attrs(&self, items: &[AttrItem], out: &mut Vec<AttrDecl>, depth: usize) {
        if depth > 32 {
            return;
        }
        for a in items {
            match a {
                AttrItem::Attr(d) => out.push(d.clone()),
                AttrItem::Ref(k, req) => {
                    let ty = self.attributes.get(k).map(|d| d.ty.clone()).unwrap_or(TypeRef::Named((XSD_NS.into(), "string".into())));
                    out.push(AttrDecl { name: k.clone(), ty, required: *req });
                }
                AttrItem::Group(k) => {
                    if let Some(g) = self.attr_groups.get(k) {
                        self.expand_attrs(g, out, depth + 1);
                    }
                }
            }
        }
    }

    fn complex_info(&self, c: &ComplexDef, depth: usize) -> TypeInfo {
        let (mut children, mut attrs, mut any) = (Vec::new(), Vec::new(), false);
        let mut simple = c.simple_content.as_ref().map(|t| self.ref_builtin(t, depth + 1));
        if let Some(b) = &c.base {
            if depth < 32 {
                if let TypeInfo::Complex { children: bc, attrs: ba, simple: bs, any: bany } = self.info_of_key(b, depth + 1) {
                    children = bc;
                    attrs = ba;
                    any = bany;
                    if simple.is_none() {
                        simple = bs;
                    }
                }
            }
        }
        self.expand_items(&c.items, false, false, &mut children, &mut any, depth);
        self.expand_attrs(&c.attrs, &mut attrs, depth);
        TypeInfo::Complex { children, attrs, simple, any }
    }

    fn info_of_key(&self, k: &Key, depth: usize) -> TypeInfo {
        if k.0 == "\u{0}head" {
            // substitution group member without its own type: the head element's type
            let hk = crate::support::parse_clark(&k.1);
            return match self.elements.get(&hk) {
                Some(h) if depth < 32 => match &h.ty {
                    TypeRef::Named(t) => self.info_of_key(t, depth + 1),
                    other => self.type_info(other),
                },
                _ => TypeInfo::Unknown,
            };
        }
        if k.0 == XSD_NS {
            return if k.1 == "anyType" { TypeInfo::Unknown } else { TypeInfo::Simple { builtin: k.1.clone(), list: false } };
        }
        match self.types.get(k) {
            Some(TypeDef::Simple(s)) => {
                let (b, l) = self.simple_builtin(s, depth + 1);
                TypeInfo::Simple { builtin: b, list: l }
            }
            Some(TypeDef::Complex(c)) => self.complex_info(c, depth + 1),
            None => TypeInfo::Unknown,
        }
    }

    /// The shape of a type reference.
    pub fn type_info(&self, t: &TypeRef) -> TypeInfo {
        match t {
            TypeRef::Named(k) => self.info_of_key(k, 0),
            TypeRef::Simple(s) => {
                let (b, l) = self.simple_builtin(s, 0);
                TypeInfo::Simple { builtin: b, list: l }
            }
            TypeRef::Complex(c) => self.complex_info(c, 0),
            TypeRef::Any => TypeInfo::Unknown,
        }
    }

    /// The shape of an element declaration (resolving `ref=`).
    pub fn element_info(&self, d: &ElementDecl) -> TypeInfo {
        let d = self.resolve_element(d);
        if d.reference {
            return TypeInfo::Unknown;
        }
        self.type_info(&d.ty)
    }

    // ------------------------------------------------------------------ XML -> Value

    /// Map an element to a value using `info` (use [`TypeInfo::Unknown`] for structural mapping).
    pub fn element_to_value(&self, doc: &Document, el: NodeId, info: &TypeInfo, o: &BindOptions) -> Value {
        if doc.attribute_ns(el, XSI_NS, "nil").map(|v| v.trim() == "true" || v.trim() == "1").unwrap_or(false) {
            return Value::Null;
        }
        if let Some(t) = doc.attribute_ns(el, XSI_NS, "type") {
            let k = resolve_qname(doc, el, t.trim());
            if k.0 == XSD_NS || self.types.contains_key(&k) {
                let derived = self.info_of_key(&k, 0);
                if !matches!(derived, TypeInfo::Unknown) {
                    let mut v = self.element_to_value_typed(doc, el, &derived, o);
                    if let Value::Object(m) = &mut v {
                        if k.0 != XSD_NS {
                            m.insert(o.type_key(), Value::String(self.type_label(&k)));
                        }
                    }
                    return v;
                }
            }
        }
        self.element_to_value_typed(doc, el, info, o)
    }

    fn element_to_value_typed(&self, doc: &Document, el: NodeId, info: &TypeInfo, o: &BindOptions) -> Value {
        match info {
            TypeInfo::Simple { builtin, list } => o.typed(&doc.string_value(el), builtin, *list),
            TypeInfo::Complex { children, attrs, simple, .. } => {
                let mut m = ObjectMap::new();
                for &a in doc.attributes(el) {
                    let n = doc.name(a);
                    if n.uri == XSI_NS || n.uri == XMLNS_NS {
                        continue;
                    }
                    let v = match attrs.iter().find(|d| d.name.1 == n.local) {
                        Some(d) => {
                            let (b, l) = self.ref_builtin(&d.ty, 0);
                            o.typed(doc.value(a), &b, l)
                        }
                        None => Value::String(doc.value(a).to_string()),
                    };
                    m.insert(format!("{}{}", o.attr_prefix, n.local), v);
                }
                if let Some((b, l)) = simple {
                    let v = o.typed(&doc.string_value(el), b, *l);
                    if attrs.is_empty() && m.is_empty() {
                        return v;
                    }
                    m.insert(o.text_key.clone(), v);
                    return Value::Object(m);
                }
                self.children_to_map(doc, el, Some(children), o, &mut m);
                Value::Object(m)
            }
            TypeInfo::Unknown => {
                let mut m = ObjectMap::new();
                for &a in doc.attributes(el) {
                    let n = doc.name(a);
                    if n.uri == XSI_NS || n.uri == XMLNS_NS {
                        continue;
                    }
                    m.insert(format!("{}{}", o.attr_prefix, n.local), Value::String(doc.value(a).to_string()));
                }
                let has_elements = doc.children(el).iter().any(|&c| doc.node_type(c) == NodeType::Element);
                if !has_elements {
                    let text = doc.string_value(el);
                    if m.is_empty() {
                        return Value::String(text);
                    }
                    if !text.trim().is_empty() {
                        m.insert(o.text_key.clone(), Value::String(text));
                    }
                    return Value::Object(m);
                }
                self.children_to_map(doc, el, None, o, &mut m);
                Value::Object(m)
            }
        }
    }

    fn children_to_map(&self, doc: &Document, el: NodeId, decls: Option<&Vec<ElementDecl>>, o: &BindOptions, m: &mut ObjectMap) {
        let kids = doc.element_children(el);
        for &c in &kids {
            let (ns, local) = (doc.namespace_uri(c), doc.local_name(c));
            let decl = decls.and_then(|ds| ds.iter().find(|d| d.name.1 == local && (d.name.0 == ns || d.reference)).or_else(|| ds.iter().find(|d| d.name.1 == local)));
            // a substitution group member standing for a declared head
            let member_of = if decl.is_none() {
                let k = (ns.to_string(), local.to_string());
                decls.and_then(|ds| ds.iter().find(|d| self.substitution_members(&self.resolve_element(d).name).contains(&k)))
            } else {
                None
            };
            let (info, repeated) = match (decl, member_of) {
                (Some(d), _) => (self.element_info(d), d.repeated()),
                (None, Some(head)) => (self.global_element(ns, local).map(|g| self.element_info(g)).unwrap_or(TypeInfo::Unknown), head.repeated()),
                (None, None) => {
                    let info = match self.global_element(ns, local) {
                        Some(g) => self.element_info(g),
                        None => TypeInfo::Unknown,
                    };
                    (info, kids.iter().filter(|&&k| doc.local_name(k) == local).count() > 1)
                }
            };
            let v = self.element_to_value(doc, c, &info, o);
            if repeated {
                match m.get_mut(local) {
                    Some(Value::Array(a)) => a.push(v),
                    _ => {
                        m.insert(local.to_string(), Value::Array(vec![v]));
                    }
                }
            } else {
                m.insert(local.to_string(), v);
            }
        }
    }

    // ------------------------------------------------------------------ Value -> XML

    /// Write `v` as an element named `name` under `parent` of the writer's document.
    /// Arrays write one element per item.
    pub fn write_value(&self, w: &mut XmlWriter, parent: NodeId, name: &Key, v: &Value, info: &TypeInfo, nillable: bool, o: &BindOptions) -> Result<()> {
        if let Value::Array(items) = v {
            let list = matches!(info, TypeInfo::Simple { list: true, .. });
            if !list {
                for it in items {
                    self.write_value(w, parent, name, it, info, nillable, o)?;
                }
                return Ok(());
            }
        }
        if v.is_null() && !nillable && parent != 0 {
            return Ok(());
        }
        let el = w.element(parent, name);
        if v.is_null() {
            let q = w.qname(&(XSI_NS.to_string(), "nil".to_string()), true);
            w.doc.set_attribute(el, q, "true");
            return Ok(());
        }
        // xsi:type polymorphism
        let type_key = o.type_key();
        let mut derived = None;
        if let Value::Object(m) = v {
            if let Some(t) = m.get(&type_key).map(|t| t.to_string()) {
                let k = self.find_type(&t, &o.prefixes).ok_or_else(|| CamelError::illegal_argument(format!("Unknown xsi:type '{}' for element {}", t, name.1)))?;
                let tq = w.qname(&k, true);
                let lexical = if tq.prefix.is_empty() { tq.local.clone() } else { format!("{}:{}", tq.prefix, tq.local) };
                let q = w.qname(&(XSI_NS.to_string(), "type".to_string()), true);
                w.doc.set_attribute(el, q, lexical);
                derived = Some(self.info_of_key(&k, 0));
            }
        }
        let info = derived.as_ref().unwrap_or(info);
        match info {
            TypeInfo::Simple { builtin, .. } => {
                w.doc.append_text(el, &value_text(v, builtin));
            }
            TypeInfo::Complex { children, attrs, simple, .. } => {
                let Value::Object(m) = v else {
                    if let Some((b, _)) = simple {
                        w.doc.append_text(el, &value_text(v, b));
                        return Ok(());
                    }
                    if children.len() == 1 {
                        // a scalar for a single-child wrapper
                        let d = self.resolve_element(&children[0]);
                        let ci = self.element_info(&d);
                        return self.write_value(w, el, &d.name, v, &ci, d.nillable, o);
                    }
                    return Err(CamelError::illegal_argument(format!("Cannot bind {} value to complex element {}", v.type_name(), name.1)));
                };
                let mut used: Vec<&str> = vec![type_key.as_str()];
                for a in attrs {
                    let pk = format!("{}{}", o.attr_prefix, a.name.1);
                    let found = m.get_key_value(&pk).or_else(|| if simple.is_some() || children.iter().all(|c| c.name.1 != a.name.1) { m.get_key_value(&a.name.1) } else { None });
                    if let Some((k, av)) = found {
                        used.push(k.as_str());
                        if !av.is_null() {
                            let (b, _) = self.ref_builtin(&a.ty, 0);
                            let q = w.qname(&a.name, true);
                            w.doc.set_attribute(el, q, value_text(av, &b));
                        }
                    }
                }
                if let Some((b, _)) = simple {
                    if let Some((k, tv)) = m.get_key_value(&o.text_key) {
                        used.push(k.as_str());
                        w.doc.append_text(el, &value_text(tv, b));
                    }
                }
                for d in children {
                    let d = self.resolve_element(d);
                    if let Some((k, cv)) = m.get_key_value(&d.name.1) {
                        used.push(k.as_str());
                        let ci = self.element_info(&d);
                        self.write_value(w, el, &d.name, cv, &ci, d.nillable, o)?;
                    } else {
                        for mk in self.substitution_members(&d.name) {
                            if let Some((k, cv)) = m.get_key_value(&mk.1) {
                                used.push(k.as_str());
                                let g = self.global_element(&mk.0, &mk.1).cloned();
                                let ci = g.as_ref().map(|g| self.element_info(g)).unwrap_or(TypeInfo::Unknown);
                                self.write_value(w, el, &mk, cv, &ci, g.map(|g| g.nillable).unwrap_or(false), o)?;
                            }
                        }
                    }
                }
                let rest: Vec<(&String, &Value)> = m.iter().filter(|(k, _)| !used.contains(&k.as_str())).collect();
                self.write_structural_entries(w, el, &name.0, rest, o)?;
            }
            TypeInfo::Unknown => match v {
                Value::Object(m) => {
                    let entries: Vec<(&String, &Value)> = m.iter().collect();
                    self.write_structural_entries(w, el, &name.0, entries, o)?;
                }
                other => w.doc.append_text(el, &value_text(other, "")),
            },
        }
        Ok(())
    }

    fn write_structural_entries(&self, w: &mut XmlWriter, el: NodeId, ns: &str, entries: Vec<(&String, &Value)>, o: &BindOptions) -> Result<()> {
        for (k, cv) in entries {
            if !o.attr_prefix.is_empty() && k.starts_with(&o.attr_prefix) {
                if !cv.is_null() {
                    let q = QName::local(&k[o.attr_prefix.len()..]);
                    w.doc.set_attribute(el, q, value_text(cv, ""));
                }
            } else if *k == o.text_key && !matches!(cv, Value::Object(_) | Value::Array(_)) {
                w.doc.append_text(el, &value_text(cv, ""));
            } else {
                let key = (ns.to_string(), k.clone());
                let info = match self.global_element(ns, k) {
                    Some(g) => self.element_info(g),
                    None => TypeInfo::Unknown,
                };
                self.write_value(w, el, &key, cv, &info, false, o)?;
            }
        }
        Ok(())
    }
}

/// Builds a document assigning namespace prefixes.
pub struct XmlWriter {
    pub doc: Document,
    root: Option<NodeId>,
    prefixes: Vec<(String, String)>,
    preferred: Vec<(String, String)>,
    counter: usize,
}

impl XmlWriter {
    /// A writer with preferred `(uri, prefix)` bindings.
    pub fn new(preferred: &[(String, String)]) -> XmlWriter {
        XmlWriter { doc: Document::new(), root: None, prefixes: Vec::new(), preferred: preferred.to_vec(), counter: 1 }
    }

    /// The QName for an expanded name (declaring the prefix on the root element).
    pub fn qname(&mut self, k: &Key, attribute: bool) -> QName {
        if k.0.is_empty() {
            return QName::local(&k.1);
        }
        let _ = attribute;
        if let Some((_, p)) = self.prefixes.iter().find(|(u, _)| *u == k.0) {
            return QName::ns(p.clone(), &k.1, &k.0);
        }
        let p = if k.0 == XSI_NS {
            "xsi".to_string()
        } else if let Some((_, p)) = self.preferred.iter().find(|(u, _)| *u == k.0) {
            p.clone()
        } else {
            loop {
                let p = format!("ns{}", self.counter);
                self.counter += 1;
                if !self.preferred.iter().any(|(_, x)| *x == p) {
                    break p;
                }
            }
        };
        self.prefixes.push((k.0.clone(), p.clone()));
        if let Some(r) = self.root {
            self.doc.declare_namespace(r, &p, &k.0);
        }
        QName::ns(p, &k.1, &k.0)
    }

    /// Append an element.
    pub fn element(&mut self, parent: NodeId, name: &Key) -> NodeId {
        let q = self.qname(name, false);
        let el = self.doc.append_element(parent, q);
        if self.root.is_none() {
            self.root = Some(el);
            for (u, p) in self.prefixes.clone() {
                self.doc.declare_namespace(el, &p, &u);
            }
        }
        el
    }

    /// The root element.
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }
}

const INT_TYPES: &[&str] = &[
    "integer",
    "int",
    "long",
    "short",
    "byte",
    "nonNegativeInteger",
    "positiveInteger",
    "negativeInteger",
    "nonPositiveInteger",
    "unsignedLong",
    "unsignedInt",
    "unsignedShort",
    "unsignedByte",
];

/// Convert lexical text of a built-in type to a value.
pub fn typed_text(text: &str, builtin: &str, list: bool) -> Value {
    if list {
        return Value::Array(text.split_whitespace().map(|t| typed_text(t, builtin, false)).collect());
    }
    let t = text.trim();
    if INT_TYPES.contains(&builtin) {
        return t.trim_start_matches('+').parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::String(t.to_string()));
    }
    match builtin {
        "decimal" | "float" | "double" => match t {
            "INF" => Value::Float(f64::INFINITY),
            "-INF" => Value::Float(f64::NEG_INFINITY),
            "NaN" => Value::Float(f64::NAN),
            _ => t.parse::<f64>().map(Value::Float).unwrap_or_else(|_| Value::String(t.to_string())),
        },
        "boolean" => match t {
            "true" | "1" => Value::Bool(true),
            "false" | "0" => Value::Bool(false),
            _ => Value::String(t.to_string()),
        },
        "base64Binary" => camel_tls::pem::base64_decode(t).map(Value::Bytes).unwrap_or_else(|| Value::String(t.to_string())),
        "hexBinary" => {
            let ok = t.len() % 2 == 0 && t.bytes().all(|b| b.is_ascii_hexdigit());
            if ok {
                Value::Bytes((0..t.len()).step_by(2).map(|i| u8::from_str_radix(&t[i..i + 2], 16).unwrap_or(0)).collect())
            } else {
                Value::String(t.to_string())
            }
        }
        "string" | "normalizedString" | "anySimpleType" | "" => Value::String(text.to_string()),
        _ => Value::String(t.to_string()),
    }
}

// ------------------------------------------------------------------ typed dates

fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Split a trailing time zone (`Z`, `+hh:mm`, `-hh:mm`) off; returns (rest, minutes).
fn split_zone(s: &str) -> (&str, Option<i64>) {
    if let Some(r) = s.strip_suffix('Z') {
        return (r, Some(0));
    }
    if s.len() > 6 {
        let (r, z) = s.split_at(s.len() - 6);
        let zb = z.as_bytes();
        if (zb[0] == b'+' || zb[0] == b'-') && zb[3] == b':' {
            if let (Some(h), Some(m)) = (num::<i64>(&z[1..3]), num::<i64>(&z[4..6])) {
                let mins = h * 60 + m;
                return (r, Some(if zb[0] == b'-' { -mins } else { mins }));
            }
        }
    }
    (s, None)
}

fn parse_date_part(s: &str, m: &mut ObjectMap) -> Option<()> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let mut p = s.splitn(3, '-');
    let (y, mo, d) = (p.next()?, p.next()?, p.next()?);
    if y.len() < 4 || mo.len() != 2 || d.len() != 2 {
        return None;
    }
    let y: i64 = num(y)?;
    m.insert("year".into(), Value::Int(if neg { -y } else { y }));
    m.insert("month".into(), Value::Int(num(mo)?));
    m.insert("day".into(), Value::Int(num(d)?));
    Some(())
}

fn parse_time_part(s: &str, m: &mut ObjectMap) -> Option<()> {
    let mut p = s.splitn(3, ':');
    let (h, mi, sec) = (p.next()?, p.next()?, p.next()?);
    if h.len() != 2 || mi.len() != 2 {
        return None;
    }
    m.insert("hour".into(), Value::Int(num(h)?));
    m.insert("minute".into(), Value::Int(num(mi)?));
    let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
    if whole.len() != 2 {
        return None;
    }
    m.insert("second".into(), Value::Int(num(whole)?));
    if !frac.is_empty() {
        let ms: String = frac.chars().chain("000".chars()).take(3).collect();
        m.insert("millisecond".into(), Value::Int(num(&ms)?));
    }
    Some(())
}

/// The object form of a `date` / `dateTime` / `time` / `duration` lexical value.
pub fn temporal_value(text: &str, builtin: &str) -> Option<Value> {
    let mut m = ObjectMap::new();
    match builtin {
        "date" => {
            let (rest, tz) = split_zone(text);
            parse_date_part(rest, &mut m)?;
            if let Some(z) = tz {
                m.insert("timezone".into(), Value::Int(z));
            }
        }
        "dateTime" | "dateTimeStamp" => {
            let (rest, tz) = split_zone(text);
            let (d, t) = rest.split_once('T')?;
            parse_date_part(d, &mut m)?;
            parse_time_part(t, &mut m)?;
            if let Some(z) = tz {
                m.insert("timezone".into(), Value::Int(z));
            }
        }
        "time" => {
            let (rest, tz) = split_zone(text);
            parse_time_part(rest, &mut m)?;
            if let Some(z) = tz {
                m.insert("timezone".into(), Value::Int(z));
            }
        }
        "duration" => {
            let (neg, s) = match text.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, text),
            };
            let s = s.strip_prefix('P')?;
            let (date, time) = s.split_once('T').unwrap_or((s, ""));
            m.insert("negative".into(), Value::Bool(neg));
            let mut take = |part: &str, units: &[(char, &str)]| -> Option<()> {
                let mut cur = String::new();
                for c in part.chars() {
                    if c.is_ascii_digit() || c == '.' {
                        cur.push(c);
                    } else {
                        let (_, name) = units.iter().find(|(u, _)| *u == c)?;
                        let v = if cur.contains('.') { Value::Float(cur.parse().ok()?) } else { Value::Int(num(&cur)?) };
                        m.insert((*name).into(), v);
                        cur.clear();
                    }
                }
                if cur.is_empty() {
                    Some(())
                } else {
                    None
                }
            };
            take(date, &[('Y', "years"), ('M', "months"), ('D', "days")])?;
            take(time, &[('H', "hours"), ('M', "minutes"), ('S', "seconds")])?;
        }
        _ => return None,
    }
    Some(Value::Object(m))
}

fn days_to_civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn zone_text(m: &ObjectMap) -> String {
    match m.get("timezone") {
        Some(Value::Int(0)) => "Z".into(),
        Some(Value::Int(z)) => format!("{}{:02}:{:02}", if *z < 0 { '-' } else { '+' }, z.abs() / 60, z.abs() % 60),
        _ => String::new(),
    }
}

/// Lexical form of an object / epoch-millis value for a temporal built-in type.
fn temporal_text(v: &Value, builtin: &str) -> Option<String> {
    let int = |m: &ObjectMap, k: &str| m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    match (v, builtin) {
        (Value::Int(ms), "dateTime" | "dateTimeStamp" | "date") => {
            let secs = ms.div_euclid(1000);
            let (y, mo, d) = days_to_civil(secs.div_euclid(86_400));
            if builtin == "date" {
                return Some(format!("{:04}-{:02}-{:02}Z", y, mo, d));
            }
            let sod = secs.rem_euclid(86_400);
            Some(format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, sod / 3600, sod / 60 % 60, sod % 60, ms.rem_euclid(1000)))
        }
        (Value::Object(m), "date" | "dateTime" | "dateTimeStamp" | "time") if m.contains_key("year") || m.contains_key("hour") => {
            let date = || format!("{}{:04}-{:02}-{:02}", if int(m, "year") < 0 { "-" } else { "" }, int(m, "year").abs(), int(m, "month"), int(m, "day"));
            let time = || {
                let mut t = format!("{:02}:{:02}:{:02}", int(m, "hour"), int(m, "minute"), int(m, "second"));
                if let Some(ms) = m.get("millisecond").and_then(|v| v.as_i64()) {
                    t.push_str(&format!(".{:03}", ms));
                }
                t
            };
            Some(match builtin {
                "date" => format!("{}{}", date(), zone_text(m)),
                "time" => format!("{}{}", time(), zone_text(m)),
                _ => format!("{}T{}{}", date(), time(), zone_text(m)),
            })
        }
        (Value::Object(m), "duration") => {
            let mut s = String::new();
            if m.get("negative").and_then(|v| v.as_bool()).unwrap_or(false) {
                s.push('-');
            }
            s.push('P');
            let part = |k: &str| m.get(k).filter(|v| !v.is_null()).map(|v| v.to_string());
            for (k, u) in [("years", 'Y'), ("months", 'M'), ("days", 'D')] {
                if let Some(x) = part(k) {
                    s.push_str(&format!("{}{}", x, u));
                }
            }
            let t: Vec<String> = [("hours", 'H'), ("minutes", 'M'), ("seconds", 'S')].iter().filter_map(|(k, u)| part(k).map(|x| format!("{}{}", x, u))).collect();
            if !t.is_empty() {
                s.push('T');
                s.push_str(&t.concat());
            }
            if s.ends_with('P') {
                s.push_str("T0S");
            }
            Some(s)
        }
        _ => None,
    }
}

/// Lexical text of a value for a built-in type.
pub fn value_text(v: &Value, builtin: &str) -> String {
    if let Some(t) = temporal_text(v, builtin) {
        return t;
    }
    match v {

        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_nan() {
                "NaN".into()
            } else if f.is_infinite() {
                if *f > 0.0 { "INF".into() } else { "-INF".into() }
            } else if INT_TYPES.contains(&builtin) && f.fract() == 0.0 {
                format!("{}", *f as i64)
            } else {
                format!("{}", f)
            }
        }
        Value::String(s) => s.clone(),
        Value::Bytes(b) => {
            if builtin == "hexBinary" {
                b.iter().map(|x| format!("{:02X}", x)).collect()
            } else {
                camel_tls::pem::base64_encode(b)
            }
        }
        Value::Array(a) => a.iter().map(|x| value_text(x, builtin)).collect::<Vec<_>>().join(" "),
        Value::Object(_) => v.to_json(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:o" xmlns:o="urn:o" elementFormDefault="qualified">
      <xs:element name="order" type="o:Order"/>
      <xs:complexType name="Base"><xs:sequence><xs:element name="id" type="xs:long"/></xs:sequence></xs:complexType>
      <xs:complexType name="Order"><xs:complexContent><xs:extension base="o:Base"><xs:sequence>
        <xs:element name="paid" type="xs:boolean"/>
        <xs:element name="line" maxOccurs="unbounded"><xs:complexType><xs:sequence>
            <xs:element name="qty" type="o:Qty"/><xs:element name="price" type="xs:decimal"/>
        </xs:sequence><xs:attribute name="sku" type="xs:string" use="required"/></xs:complexType></xs:element>
        <xs:element name="note" type="xs:string" minOccurs="0" nillable="true"/>
        <xs:element name="amount" minOccurs="0"><xs:complexType><xs:simpleContent><xs:extension base="xs:decimal"><xs:attribute name="currency" type="xs:string"/></xs:extension></xs:simpleContent></xs:complexType></xs:element>
      </xs:sequence></xs:extension></xs:complexContent></xs:complexType>
      <xs:simpleType name="Qty"><xs:restriction base="xs:int"><xs:minInclusive value="1"/></xs:restriction></xs:simpleType>
    </xs:schema>"#;

    #[test]
    fn typed_round_trip() {
        let s = SchemaSet::parse(XSD, None).unwrap();
        let xml = r#"<o:order xmlns:o="urn:o"><o:id>7</o:id><o:paid>true</o:paid><o:line sku="A"><o:qty>2</o:qty><o:price>1.5</o:price></o:line><o:note xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:nil="true"/><o:amount currency="EUR">3.0</o:amount></o:order>"#;
        let doc = Document::parse(xml).unwrap();
        let g = s.global_element("urn:o", "order").unwrap().clone();
        let info = s.element_info(&g);
        let o = BindOptions::default();
        let v = s.element_to_value(&doc, doc.document_element().unwrap(), &info, &o);
        assert_eq!(v.get("id"), Some(&Value::Int(7)));
        assert_eq!(v.get("paid"), Some(&Value::Bool(true)));
        let lines = v.get("line").unwrap().as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].get("@sku"), Some(&Value::from("A")));
        assert_eq!(lines[0].get("qty"), Some(&Value::Int(2)));
        assert_eq!(lines[0].get("price"), Some(&Value::Float(1.5)));
        assert_eq!(v.get("note"), Some(&Value::Null));
        assert_eq!(v.get("amount").unwrap().get("value"), Some(&Value::Float(3.0)));
        let mut w = XmlWriter::new(&[("urn:o".into(), "o".into())]);
        s.write_value(&mut w, 0, &g.name, &v, &info, false, &o).unwrap();
        assert_eq!(
            w.doc.to_xml_string(),
            r#"<o:order xmlns:o="urn:o" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><o:id>7</o:id><o:paid>true</o:paid><o:line sku="A"><o:qty>2</o:qty><o:price>1.5</o:price></o:line><o:note xsi:nil="true"/><o:amount currency="EUR">3</o:amount></o:order>"#
        );
    }

    const POLY: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:z" xmlns:z="urn:z" elementFormDefault="qualified">
      <xs:element name="zoo"><xs:complexType><xs:sequence>
        <xs:element name="animal" type="z:Animal" maxOccurs="unbounded"/>
        <xs:element ref="z:shape" minOccurs="0" maxOccurs="unbounded"/>
        <xs:element name="opened" type="xs:date"/><xs:element name="fed" type="xs:dateTime"/><xs:element name="stay" type="xs:duration"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:complexType name="Animal"><xs:sequence><xs:element name="name" type="xs:string"/></xs:sequence></xs:complexType>
      <xs:complexType name="Dog"><xs:complexContent><xs:extension base="z:Animal"><xs:sequence><xs:element name="barks" type="xs:int"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
      <xs:element name="shape" type="z:Shape" abstract="true"/>
      <xs:complexType name="Shape"><xs:sequence><xs:element name="color" type="xs:string"/></xs:sequence></xs:complexType>
      <xs:element name="circle" substitutionGroup="z:shape"><xs:complexType><xs:complexContent><xs:extension base="z:Shape"><xs:sequence><xs:element name="radius" type="xs:double"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element>
      <xs:element name="square" substitutionGroup="z:shape"/>
    </xs:schema>"#;

    #[test]
    fn xsi_type_substitution_groups_and_typed_dates() {
        let s = SchemaSet::parse(POLY, None).unwrap();
        let xml = r#"<z:zoo xmlns:z="urn:z" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><z:animal><z:name>generic</z:name></z:animal><z:animal xsi:type="z:Dog"><z:name>Rex</z:name><z:barks>3</z:barks></z:animal><z:circle><z:color>red</z:color><z:radius>2.5</z:radius></z:circle><z:square><z:color>blue</z:color></z:square><z:opened>2024-05-01+02:00</z:opened><z:fed>2024-05-01T10:20:30.5Z</z:fed><z:stay>P1DT2H</z:stay></z:zoo>"#;
        let doc = Document::parse(xml).unwrap();
        let g = s.global_element("urn:z", "zoo").unwrap().clone();
        let info = s.element_info(&g);
        let o = BindOptions { typed_dates: true, prefixes: vec![("urn:z".into(), "z".into())], ..Default::default() };
        let v = s.element_to_value(&doc, doc.document_element().unwrap(), &info, &o);
        let animals = v.get("animal").unwrap().as_array().unwrap();
        assert_eq!(animals[1].get("@xsi:type"), Some(&Value::from("Dog")));
        assert_eq!(animals[1].get("barks"), Some(&Value::Int(3)));
        assert_eq!(v.get("circle").unwrap().as_array().unwrap()[0].get("radius"), Some(&Value::Float(2.5)));
        assert_eq!(v.get("square").unwrap().as_array().unwrap()[0].get("color"), Some(&Value::from("blue")));
        assert_eq!(v.get("opened").unwrap().to_json(), r#"{"year":2024,"month":5,"day":1,"timezone":120}"#);
        assert_eq!(v.get("fed").unwrap().get("millisecond"), Some(&Value::Int(500)));
        assert_eq!(v.get("stay").unwrap().to_json(), r#"{"negative":false,"days":1,"hours":2}"#);
        let mut w = XmlWriter::new(&o.prefixes);
        s.write_value(&mut w, 0, &g.name, &v, &info, false, &o).unwrap();
        assert_eq!(
            w.doc.to_xml_string(),
            r#"<z:zoo xmlns:z="urn:z" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><z:animal><z:name>generic</z:name></z:animal><z:animal xsi:type="z:Dog"><z:name>Rex</z:name><z:barks>3</z:barks></z:animal><z:circle><z:color>red</z:color><z:radius>2.5</z:radius></z:circle><z:square><z:color>blue</z:color></z:square><z:opened>2024-05-01+02:00</z:opened><z:fed>2024-05-01T10:20:30.500Z</z:fed><z:stay>P1DT2H</z:stay></z:zoo>"#
        );
        assert_eq!(value_text(&Value::Int(86_400_000 + 1), "dateTime"), "1970-01-02T00:00:00.001Z");
        assert_eq!(value_text(&Value::Int(0), "date"), "1970-01-01Z");
    }

    #[test]
    fn structural_fallback() {
        let s = SchemaSet::new();
        let doc = Document::parse("<a x='1'><b>1</b><b>2</b><c><d>t</d></c></a>").unwrap();
        let v = s.element_to_value(&doc, doc.document_element().unwrap(), &TypeInfo::Unknown, &BindOptions::default());
        assert_eq!(v.to_json(), r#"{"@x":"1","b":["1","2"],"c":{"d":"t"}}"#);
    }
}
