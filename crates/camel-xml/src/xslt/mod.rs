//! XSLT 1.0 processor.
//!
//! Supported: `xsl:stylesheet`/`xsl:transform` (and simplified literal-result-element
//! stylesheets), `include`/`import` (import precedence, `apply-imports`), `template`
//! (match patterns with default priorities, `name`, `mode`, `priority`), built-in template
//! rules, `apply-templates` (select, mode, sort, with-param), `call-template`, `param` /
//! `with-param`, `variable` (global variables are evaluated lazily, local scoping),
//! `value-of`, `text`, `for-each`, `if`, `choose`/`when`/`otherwise`, `sort` (text / number,
//! ascending / descending, case-order), `copy`, `copy-of`, `element`, `attribute`,
//! `attribute-set` / `use-attribute-sets`, `comment`, `processing-instruction`, `number`
//! (level single / multiple / any, count, from, value, format, grouping), `message`
//! (terminate), `fallback`, `key` + `key()`, `decimal-format` + `format-number()`,
//! `strip-space` / `preserve-space`, `namespace-alias`, `output` (method xml / html / text,
//! indent, encoding, omit-xml-declaration, standalone, doctype-public / -system,
//! cdata-section-elements, media-type, `xalan:indent-amount`), `disable-output-escaping`,
//! literal result elements with attribute value templates, `exclude-result-prefixes`.
//! Functions: `document()` (files relative to the stylesheet, `document('')`), `key()`,
//! `format-number()`, `current()`, `generate-id()`, `system-property()`,
//! `element-available()`, `function-available()`, `unparsed-entity-uri()`, and
//! `exsl:node-set()` / `msxsl:node-set()` (result tree fragments may also be used as
//! node-sets directly).
//!
//! XSLT 2.0 / 3.0 instructions (`xsl:for-each-group`, `xsl:function`, `xsl:sequence`,
//! `xsl:analyze-string`, `xsl:result-document`, ...) without an `xsl:fallback` are rejected
//! with a clear error, as are XPath 2.0 expressions.

mod exec;
pub mod number;
pub mod pattern;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use camel_core::error::CamelError;

use crate::dom::{Document, NodeId, NodeType, QName, XML_NS};
use crate::serialize::{Method, OutputOptions};
use crate::xpath::XPath;
use number::DecimalFormat;
use pattern::Pattern;

pub use exec::TransformResult;

/// The XSLT namespace.
pub const XSLT_NS: &str = "http://www.w3.org/1999/XSL/Transform";
const XALAN_NS: [&str; 2] = ["http://xml.apache.org/xalan", "http://xml.apache.org/xslt"];

/// An expanded name `(namespace URI, local name)`.
pub type Key = (String, String);

/// Attribute value template part.
#[derive(Clone, Debug)]
pub(crate) enum AvtPart {
    Lit(String),
    Expr(XPath),
}

/// An attribute value template.
#[derive(Clone, Debug)]
pub(crate) struct Avt(pub Vec<AvtPart>);

#[derive(Clone, Debug)]
pub(crate) enum VarValue {
    Select(XPath),
    Body(Vec<Instr>),
    Empty,
}

#[derive(Clone, Debug)]
pub(crate) struct WithParam {
    pub name: Key,
    pub value: VarValue,
}

#[derive(Clone, Debug)]
pub(crate) struct SortSpec {
    pub select: XPath,
    pub data_type: Avt,
    pub order: Avt,
    pub case_order: Avt,
}

#[derive(Clone, Debug)]
pub(crate) struct NumberSpec {
    pub level: String,
    pub count: Option<Pattern>,
    pub from: Option<Pattern>,
    pub value: Option<XPath>,
    pub format: Avt,
    pub grouping_separator: Option<Avt>,
    pub grouping_size: Option<Avt>,
}

#[derive(Clone, Debug)]
pub(crate) enum Instr {
    Text(String),
    RawText(String),
    ValueOf { select: XPath, raw: bool },
    LiteralElement { name: QName, ns: Vec<(String, String)>, attrs: Vec<(QName, Avt)>, use_sets: Vec<Key>, body: Vec<Instr> },
    Element { name: Avt, namespace: Option<Avt>, ns_ctx: Vec<(String, String)>, use_sets: Vec<Key>, body: Vec<Instr> },
    Attribute { name: Avt, namespace: Option<Avt>, ns_ctx: Vec<(String, String)>, body: Vec<Instr> },
    Comment(Vec<Instr>),
    Pi { name: Avt, body: Vec<Instr> },
    ApplyTemplates { select: Option<XPath>, mode: Option<Key>, sorts: Vec<SortSpec>, params: Vec<WithParam> },
    CallTemplate { name: Key, params: Vec<WithParam> },
    ApplyImports,
    ForEach { select: XPath, sorts: Vec<SortSpec>, body: Vec<Instr> },
    If { test: XPath, body: Vec<Instr> },
    Choose { whens: Vec<(XPath, Vec<Instr>)>, otherwise: Option<Vec<Instr>> },
    Variable { name: Key, value: VarValue },
    Copy { use_sets: Vec<Key>, body: Vec<Instr> },
    CopyOf(XPath),
    Number(Box<NumberSpec>),
    Message { terminate: Avt, body: Vec<Instr> },
    /// An instruction that is not supported; fails when executed.
    Unsupported(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub name: Key,
    pub value: VarValue,
}

#[derive(Debug)]
pub(crate) struct TemplateRule {
    pub pattern: Option<pattern::PathPattern>,
    pub name: Option<Key>,
    pub mode: Option<Key>,
    pub priority: f64,
    pub precedence: usize,
    pub order: usize,
    pub params: Arc<Vec<Param>>,
    pub body: Arc<Vec<Instr>>,
}

#[derive(Debug)]
pub(crate) struct KeyDef {
    pub name: Key,
    pub pattern: Pattern,
    pub use_expr: XPath,
}

#[derive(Debug)]
pub(crate) struct Global {
    pub name: Key,
    pub value: VarValue,
    pub is_param: bool,
    pub precedence: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct SpaceRule {
    /// None = `*`, Some((uri, None)) = `p:*`, Some((uri, Some(local))) = QName.
    pub test: Option<(String, Option<String>)>,
    pub strip: bool,
    pub precedence: usize,
}

/// A compiled stylesheet (thread-safe, reusable).
#[derive(Debug)]
pub struct Stylesheet {
    pub(crate) templates: Vec<TemplateRule>,
    pub(crate) named: HashMap<Key, usize>,
    pub(crate) globals: Vec<Global>,
    pub(crate) keys: Vec<KeyDef>,
    pub(crate) output: OutputOptions,
    pub(crate) space: Vec<SpaceRule>,
    pub(crate) decimal_formats: HashMap<Key, DecimalFormat>,
    pub(crate) attribute_sets: HashMap<Key, (Vec<Key>, Vec<Instr>)>,
    pub(crate) base_dir: Option<PathBuf>,
    pub(crate) source: Arc<Document>,
    pub(crate) version: f64,
}

fn cerr(m: impl Into<String>) -> CamelError {
    crate::register_kinds();
    CamelError::new(crate::TRANSFORMER_CONFIGURATION_EXCEPTION, m.into())
}

const XSLT2_INSTRUCTIONS: &[&str] = &[
    "for-each-group",
    "function",
    "sequence",
    "analyze-string",
    "matching-substring",
    "non-matching-substring",
    "result-document",
    "perform-sort",
    "next-match",
    "namespace",
    "document",
    "character-map",
    "output-character",
    "import-schema",
    "try",
    "catch",
    "iterate",
    "break",
    "next-iteration",
    "on-completion",
    "map",
    "map-entry",
    "array",
    "evaluate",
    "where-populated",
    "on-empty",
    "on-non-empty",
    "merge",
    "fork",
    "accumulator",
    "mode",
    "package",
    "use-package",
    "expose",
    "accept",
    "override",
    "global-context-item",
    "context-item",
    "source-document",
    "assert",
];

/// Compiler state.
struct Compiler {
    templates: Vec<TemplateRule>,
    globals: Vec<Global>,
    keys: Vec<KeyDef>,
    output: OutputOptions,
    output_method_set: bool,
    space: Vec<SpaceRule>,
    decimal_formats: HashMap<Key, DecimalFormat>,
    attribute_sets: HashMap<Key, (Vec<Key>, Vec<Instr>)>,
    aliases: Vec<(String, String, String)>,
    next_precedence: usize,
    order: usize,
    version: f64,
    loading: Vec<PathBuf>,
}

/// In-scope namespaces of a stylesheet element as a prefix resolver.
fn resolver<'a>(doc: &'a Document, el: NodeId) -> impl Fn(&str) -> Option<String> + 'a {
    move |p: &str| if p.is_empty() { None } else { doc.lookup_namespace(el, p) }
}

impl Compiler {
    fn xpath(&self, doc: &Document, el: NodeId, text: &str) -> Result<XPath, CamelError> {
        let r = resolver(doc, el);
        XPath::compile_with(text, &r).map_err(|e| {
            let hint = if self.version >= 2.0 { " (XPath 2.0 expressions are not supported: rust-camel implements XSLT/XPath 1.0)" } else { "" };
            cerr(format!("{}{} in <{}> (line {})", e.message, hint, doc.qualified_name(el), doc.node(el).line))
        })
    }

    fn pattern(&self, doc: &Document, el: NodeId, text: &str) -> Result<Pattern, CamelError> {
        let r = resolver(doc, el);
        Pattern::parse(text, &r).map_err(|e| cerr(format!("{} in <{}> (line {})", e, doc.qualified_name(el), doc.node(el).line)))
    }

    fn qname(&self, doc: &Document, el: NodeId, q: &str) -> Result<Key, CamelError> {
        let q = q.trim();
        match q.split_once(':') {
            Some((p, l)) => match doc.lookup_namespace(el, p) {
                Some(u) => Ok((u, l.to_string())),
                None => Err(cerr(format!("Undeclared namespace prefix '{}' in QName '{}'", p, q))),
            },
            None => Ok((String::new(), q.to_string())),
        }
    }

    fn qnames(&self, doc: &Document, el: NodeId, list: Option<&str>) -> Result<Vec<Key>, CamelError> {
        let mut v = Vec::new();
        if let Some(l) = list {
            for q in l.split_whitespace() {
                v.push(self.qname(doc, el, q)?);
            }
        }
        Ok(v)
    }

    fn avt(&self, doc: &Document, el: NodeId, text: &str) -> Result<Avt, CamelError> {
        let c: Vec<char> = text.chars().collect();
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut i = 0;
        while i < c.len() {
            match c[i] {
                '{' if c.get(i + 1) == Some(&'{') => {
                    lit.push('{');
                    i += 2;
                }
                '}' if c.get(i + 1) == Some(&'}') => {
                    lit.push('}');
                    i += 2;
                }
                '{' => {
                    let mut j = i + 1;
                    let mut quote: Option<char> = None;
                    while j < c.len() {
                        match quote {
                            Some(q) if c[j] == q => quote = None,
                            Some(_) => {}
                            None if c[j] == '\'' || c[j] == '"' => quote = Some(c[j]),
                            None if c[j] == '}' => break,
                            None => {}
                        }
                        j += 1;
                    }
                    if j >= c.len() {
                        return Err(cerr(format!("Unterminated attribute value template: {}", text)));
                    }
                    if !lit.is_empty() {
                        parts.push(AvtPart::Lit(std::mem::take(&mut lit)));
                    }
                    let e: String = c[i + 1..j].iter().collect();
                    parts.push(AvtPart::Expr(self.xpath(doc, el, &e)?));
                    i = j + 1;
                }
                '}' => return Err(cerr(format!("Unmatched '}}' in attribute value template: {}", text))),
                ch => {
                    lit.push(ch);
                    i += 1;
                }
            }
        }
        if !lit.is_empty() {
            parts.push(AvtPart::Lit(lit));
        }
        Ok(Avt(parts))
    }

    fn is_xsl(doc: &Document, n: NodeId, local: &str) -> bool {
        let node = doc.node(n);
        node.ty == NodeType::Element && node.name.uri == XSLT_NS && node.name.local == local
    }

    fn preserve_space(doc: &Document, el: NodeId) -> bool {
        let mut cur = Some(el);
        while let Some(c) = cur {
            if doc.node_type(c) == NodeType::Element {
                if let Some(v) = doc.attribute_ns(c, XML_NS, "space") {
                    return v == "preserve";
                }
            }
            cur = doc.parent(c);
        }
        false
    }

    fn var_value(&mut self, doc: &Document, el: NodeId) -> Result<VarValue, CamelError> {
        if let Some(s) = doc.attribute(el, "select") {
            return Ok(VarValue::Select(self.xpath(doc, el, s)?));
        }
        let body = self.body(doc, el)?;
        Ok(if body.is_empty() && doc.children(el).is_empty() { VarValue::Empty } else { VarValue::Body(body) })
    }

    fn with_params(&mut self, doc: &Document, el: NodeId) -> Result<Vec<WithParam>, CamelError> {
        let mut v = Vec::new();
        for c in doc.element_children(el) {
            if Self::is_xsl(doc, c, "with-param") {
                let name = self.qname(doc, c, doc.attribute(c, "name").ok_or_else(|| cerr("xsl:with-param requires a name"))?)?;
                let value = self.var_value(doc, c)?;
                v.push(WithParam { name, value });
            }
        }
        Ok(v)
    }

    fn sorts(&mut self, doc: &Document, el: NodeId) -> Result<Vec<SortSpec>, CamelError> {
        let mut v = Vec::new();
        for c in doc.element_children(el) {
            if Self::is_xsl(doc, c, "sort") {
                v.push(SortSpec {
                    select: self.xpath(doc, c, doc.attribute(c, "select").unwrap_or("."))?,
                    data_type: self.avt(doc, c, doc.attribute(c, "data-type").unwrap_or("text"))?,
                    order: self.avt(doc, c, doc.attribute(c, "order").unwrap_or("ascending"))?,
                    case_order: self.avt(doc, c, doc.attribute(c, "case-order").unwrap_or("upper-first"))?,
                });
            }
        }
        Ok(v)
    }

    fn excluded_namespaces(&self, doc: &Document, el: NodeId) -> Vec<String> {
        // exclude-result-prefixes / extension-element-prefixes on ancestors
        let mut out = vec![XSLT_NS.to_string()];
        let mut cur = Some(el);
        while let Some(c) = cur {
            if doc.node_type(c) != NodeType::Element {
                break;
            }
            let is_xsl_root = Self::is_xsl(doc, c, "stylesheet") || Self::is_xsl(doc, c, "transform");
            let attrs: [Option<&str>; 2] = if is_xsl_root {
                [doc.attribute(c, "exclude-result-prefixes"), doc.attribute(c, "extension-element-prefixes")]
            } else {
                [doc.attribute_ns(c, XSLT_NS, "exclude-result-prefixes"), doc.attribute_ns(c, XSLT_NS, "extension-element-prefixes")]
            };
            for list in attrs.iter().flatten() {
                for p in list.split_whitespace() {
                    let p = if p == "#default" { "" } else { p };
                    if let Some(u) = doc.lookup_namespace(c, p) {
                        out.push(u);
                    }
                }
            }
            cur = doc.parent(c);
        }
        out
    }

    fn alias(&self, uri: &str, prefix: &str) -> (String, String) {
        for (from, to_uri, to_prefix) in &self.aliases {
            if from == uri {
                return (to_uri.clone(), to_prefix.clone());
            }
        }
        (uri.to_string(), prefix.to_string())
    }

    fn body(&mut self, doc: &Document, el: NodeId) -> Result<Vec<Instr>, CamelError> {
        let mut out = Vec::new();
        let preserve = Self::preserve_space(doc, el);
        for &c in doc.children(el) {
            let n = doc.node(c);
            match n.ty {
                NodeType::Text => {
                    if !n.value.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r')).is_empty() || preserve {
                        out.push(Instr::Text(n.value.clone()));
                    }
                }
                NodeType::Element if n.name.uri == XSLT_NS => {
                    if let Some(i) = self.instruction(doc, c)? {
                        out.push(i);
                    }
                }
                NodeType::Element => out.push(self.literal(doc, c)?),
                _ => {}
            }
        }
        Ok(out)
    }

    fn literal(&mut self, doc: &Document, el: NodeId) -> Result<Instr, CamelError> {
        let n = doc.node(el);
        let excluded = self.excluded_namespaces(doc, el);
        let (uri, prefix) = self.alias(&n.name.uri, &n.name.prefix);
        let name = QName::ns(prefix, n.name.local.clone(), uri);
        let mut ns = Vec::new();
        for (p, u) in doc.in_scope_namespaces(el) {
            if p == "xml" || excluded.contains(&u) || self.aliases.iter().any(|(f, _, _)| *f == u) {
                continue;
            }
            ns.push((p, u));
        }
        let mut attrs = Vec::new();
        let mut use_sets = Vec::new();
        for &a in &n.attrs {
            let an = doc.node(a);
            if an.name.uri == XSLT_NS {
                if an.name.local == "use-attribute-sets" {
                    use_sets = self.qnames(doc, el, Some(&an.value))?;
                }
                continue;
            }
            let (au, ap) = self.alias(&an.name.uri, &an.name.prefix);
            attrs.push((QName::ns(ap, an.name.local.clone(), au), self.avt(doc, el, &an.value)?));
        }
        let body = self.body(doc, el)?;
        Ok(Instr::LiteralElement { name, ns, attrs, use_sets, body })
    }

    fn required<'d>(doc: &'d Document, el: NodeId, attr: &str) -> Result<&'d str, CamelError> {
        doc.attribute(el, attr)
            .ok_or_else(|| cerr(format!("xsl:{} requires the '{}' attribute (line {})", doc.local_name(el), attr, doc.node(el).line)))
    }

    fn instruction(&mut self, doc: &Document, el: NodeId) -> Result<Option<Instr>, CamelError> {
        let local = doc.local_name(el).to_string();
        Ok(Some(match local.as_str() {
            "text" => {
                let s = doc.string_value(el);
                if doc.attribute(el, "disable-output-escaping") == Some("yes") {
                    Instr::RawText(s)
                } else {
                    Instr::Text(s)
                }
            }
            "value-of" => Instr::ValueOf {
                select: self.xpath(doc, el, Self::required(doc, el, "select")?)?,
                raw: doc.attribute(el, "disable-output-escaping") == Some("yes"),
            },
            "apply-templates" => Instr::ApplyTemplates {
                select: match doc.attribute(el, "select") {
                    Some(s) => Some(self.xpath(doc, el, s)?),
                    None => None,
                },
                mode: match doc.attribute(el, "mode") {
                    Some(m) => Some(self.qname(doc, el, m)?),
                    None => None,
                },
                sorts: self.sorts(doc, el)?,
                params: self.with_params(doc, el)?,
            },
            "call-template" => Instr::CallTemplate { name: self.qname(doc, el, Self::required(doc, el, "name")?)?, params: self.with_params(doc, el)? },
            "apply-imports" => Instr::ApplyImports,
            "for-each" => {
                let select = self.xpath(doc, el, Self::required(doc, el, "select")?)?;
                let sorts = self.sorts(doc, el)?;
                let body = self.body(doc, el)?;
                Instr::ForEach { select, sorts, body }
            }
            "if" => Instr::If { test: self.xpath(doc, el, Self::required(doc, el, "test")?)?, body: self.body(doc, el)? },
            "choose" => {
                let mut whens = Vec::new();
                let mut otherwise = None;
                for c in doc.element_children(el) {
                    if Self::is_xsl(doc, c, "when") {
                        whens.push((self.xpath(doc, c, Self::required(doc, c, "test")?)?, self.body(doc, c)?));
                    } else if Self::is_xsl(doc, c, "otherwise") {
                        otherwise = Some(self.body(doc, c)?);
                    }
                }
                if whens.is_empty() {
                    return Err(cerr("xsl:choose must contain at least one xsl:when"));
                }
                Instr::Choose { whens, otherwise }
            }
            "variable" | "param" => {
                let name = self.qname(doc, el, Self::required(doc, el, "name")?)?;
                Instr::Variable { name, value: self.var_value(doc, el)? }
            }
            "copy" => Instr::Copy { use_sets: self.qnames(doc, el, doc.attribute(el, "use-attribute-sets"))?, body: self.body(doc, el)? },
            "copy-of" => Instr::CopyOf(self.xpath(doc, el, Self::required(doc, el, "select")?)?),
            "element" => Instr::Element {
                name: self.avt(doc, el, Self::required(doc, el, "name")?)?,
                namespace: match doc.attribute(el, "namespace") {
                    Some(n) => Some(self.avt(doc, el, n)?),
                    None => None,
                },
                ns_ctx: doc.in_scope_namespaces(el),
                use_sets: self.qnames(doc, el, doc.attribute(el, "use-attribute-sets"))?,
                body: self.body(doc, el)?,
            },
            "attribute" => Instr::Attribute {
                name: self.avt(doc, el, Self::required(doc, el, "name")?)?,
                namespace: match doc.attribute(el, "namespace") {
                    Some(n) => Some(self.avt(doc, el, n)?),
                    None => None,
                },
                ns_ctx: doc.in_scope_namespaces(el),
                body: self.body(doc, el)?,
            },
            "comment" => Instr::Comment(self.body(doc, el)?),
            "processing-instruction" => Instr::Pi { name: self.avt(doc, el, Self::required(doc, el, "name")?)?, body: self.body(doc, el)? },
            "number" => Instr::Number(Box::new(NumberSpec {
                level: doc.attribute(el, "level").unwrap_or("single").to_string(),
                count: match doc.attribute(el, "count") {
                    Some(p) => Some(self.pattern(doc, el, p)?),
                    None => None,
                },
                from: match doc.attribute(el, "from") {
                    Some(p) => Some(self.pattern(doc, el, p)?),
                    None => None,
                },
                value: match doc.attribute(el, "value") {
                    Some(v) => Some(self.xpath(doc, el, v)?),
                    None => None,
                },
                format: self.avt(doc, el, doc.attribute(el, "format").unwrap_or("1"))?,
                grouping_separator: match doc.attribute(el, "grouping-separator") {
                    Some(v) => Some(self.avt(doc, el, v)?),
                    None => None,
                },
                grouping_size: match doc.attribute(el, "grouping-size") {
                    Some(v) => Some(self.avt(doc, el, v)?),
                    None => None,
                },
            })),
            "message" => Instr::Message { terminate: self.avt(doc, el, doc.attribute(el, "terminate").unwrap_or("no"))?, body: self.body(doc, el)? },
            "fallback" | "sort" | "with-param" | "when" | "otherwise" => return Ok(None),
            other => {
                // unknown or XSLT 2.0+ instruction: use xsl:fallback if present
                let fallbacks: Vec<NodeId> = doc.element_children(el).into_iter().filter(|&c| Self::is_xsl(doc, c, "fallback")).collect();
                if !fallbacks.is_empty() {
                    let mut body = Vec::new();
                    for f in fallbacks {
                        body.extend(self.body(doc, f)?);
                    }
                    return Ok(Some(Instr::If { test: XPath::compile("true()").unwrap(), body }));
                }
                if XSLT2_INSTRUCTIONS.contains(&other) {
                    return Err(cerr(format!(
                        "XSLT 2.0/3.0 instruction xsl:{} is not supported: rust-camel implements XSLT 1.0 (line {})",
                        other,
                        doc.node(el).line
                    )));
                }
                if self.version > 1.0 {
                    Instr::Unsupported(format!("xsl:{}", other))
                } else {
                    return Err(cerr(format!("Unknown XSLT instruction xsl:{} (line {})", other, doc.node(el).line)));
                }
            }
        }))
    }

    fn template_params(&mut self, doc: &Document, el: NodeId) -> Result<Vec<Param>, CamelError> {
        let mut v = Vec::new();
        for c in doc.element_children(el) {
            if Self::is_xsl(doc, c, "param") {
                let name = self.qname(doc, c, Self::required(doc, c, "name")?)?;
                let value = self.var_value(doc, c)?;
                v.push(Param { name, value });
            }
        }
        Ok(v)
    }

    fn template_body(&mut self, doc: &Document, el: NodeId) -> Result<Vec<Instr>, CamelError> {
        let mut out = Vec::new();
        let preserve = Self::preserve_space(doc, el);
        for &c in doc.children(el) {
            let n = doc.node(c);
            match n.ty {
                NodeType::Text => {
                    if !n.value.trim().is_empty() || preserve {
                        out.push(Instr::Text(n.value.clone()));
                    }
                }
                NodeType::Element if n.name.uri == XSLT_NS && n.name.local == "param" => {}
                NodeType::Element if n.name.uri == XSLT_NS => {
                    if let Some(i) = self.instruction(doc, c)? {
                        out.push(i);
                    }
                }
                NodeType::Element => out.push(self.literal(doc, c)?),
                _ => {}
            }
        }
        Ok(out)
    }

    fn load_file(&mut self, href: &str, base: Option<&Path>) -> Result<(Document, Option<PathBuf>), CamelError> {
        let p = resolve_href(href, base);
        if self.loading.contains(&p) {
            return Err(cerr(format!("Circular stylesheet inclusion of {}", p.display())));
        }
        let text = std::fs::read(&p).map_err(|e| cerr(format!("Cannot load stylesheet {}: {}", p.display(), e)))?;
        let doc = Document::parse_bytes(&text).map_err(|e| cerr(format!("Invalid stylesheet {}: {}", p.display(), e)))?;
        Ok((doc, p.parent().map(|d| d.to_path_buf())))
    }

    /// Compile a stylesheet module (import precedence assigned in post-order).
    fn module(&mut self, doc: &Document, base: Option<&Path>, path: Option<PathBuf>) -> Result<(), CamelError> {
        let root = doc.document_element().ok_or_else(|| cerr("Empty stylesheet"))?;
        if let Some(p) = &path {
            self.loading.push(p.clone());
        }
        let rn = doc.node(root);
        if !(rn.name.uri == XSLT_NS && (rn.name.local == "stylesheet" || rn.name.local == "transform")) {
            // simplified syntax: literal result element as stylesheet
            let v = doc.attribute_ns(root, XSLT_NS, "version");
            if v.is_none() {
                return Err(cerr(format!("Not an XSLT stylesheet: root element <{}> (expected xsl:stylesheet)", rn.name.qualified())));
            }
            self.version = v.and_then(|v| v.trim().parse().ok()).unwrap_or(1.0);
            let prec = self.next_precedence;
            self.next_precedence += 1;
            let body = vec![self.literal(doc, root)?];
            let alt = Pattern::parse("/", &|_: &str| None).unwrap().alts.remove(0);
            self.order += 1;
            self.templates.push(TemplateRule {
                pattern: Some(alt),
                name: None,
                mode: None,
                priority: 0.5,
                precedence: prec,
                order: self.order,
                params: Arc::new(Vec::new()),
                body: Arc::new(body),
            });
            return Ok(());
        }
        if let Some(v) = doc.attribute(root, "version") {
            let v: f64 = v.trim().parse().unwrap_or(1.0);
            if v > self.version {
                self.version = v;
            }
        } else {
            return Err(cerr("xsl:stylesheet requires the 'version' attribute"));
        }
        // imports first (lower precedence)
        for c in doc.element_children(root) {
            if Self::is_xsl(doc, c, "import") {
                let href = Self::required(doc, c, "href")?;
                let (d, b) = self.load_file(href, base)?;
                let p = resolve_href(href, base);
                self.module(&d, b.as_deref(), Some(p))?;
            }
        }
        let prec = self.next_precedence;
        self.next_precedence += 1;
        self.declarations(doc, root, base, prec)?;
        if path.is_some() {
            self.loading.pop();
        }
        Ok(())
    }

    fn declarations(&mut self, doc: &Document, root: NodeId, base: Option<&Path>, prec: usize) -> Result<(), CamelError> {
        // namespace aliases first (they affect literal result elements)
        for c in doc.element_children(root) {
            if Self::is_xsl(doc, c, "namespace-alias") {
                let sp = Self::required(doc, c, "stylesheet-prefix")?;
                let rp = Self::required(doc, c, "result-prefix")?;
                let sp = if sp == "#default" { "" } else { sp };
                let rp = if rp == "#default" { "" } else { rp };
                let su = doc.lookup_namespace(c, sp).unwrap_or_default();
                let ru = doc.lookup_namespace(c, rp).unwrap_or_default();
                self.aliases.push((su, ru, rp.to_string()));
            }
        }
        for c in doc.element_children(root) {
            let n = doc.node(c);
            if n.name.uri != XSLT_NS {
                continue; // top-level user-defined data elements are ignored
            }
            match n.name.local.as_str() {
                "import" | "namespace-alias" => {}
                "include" => {
                    let href = Self::required(doc, c, "href")?;
                    let (d, b) = self.load_file(href, base)?;
                    let p = resolve_href(href, base);
                    self.loading.push(p);
                    let r = d.document_element().ok_or_else(|| cerr("Empty included stylesheet"))?;
                    let res = self.declarations(&d, r, b.as_deref(), prec);
                    self.loading.pop();
                    res?;
                }
                "template" => {
                    let name = match doc.attribute(c, "name") {
                        Some(nm) => Some(self.qname(doc, c, nm)?),
                        None => None,
                    };
                    let mode = match doc.attribute(c, "mode") {
                        Some(m) => Some(self.qname(doc, c, m)?),
                        None => None,
                    };
                    let explicit_priority = match doc.attribute(c, "priority") {
                        Some(p) => Some(p.trim().parse::<f64>().map_err(|_| cerr(format!("Invalid template priority: {}", p)))?),
                        None => None,
                    };
                    let params = Arc::new(self.template_params(doc, c)?);
                    let body = Arc::new(self.template_body(doc, c)?);
                    match doc.attribute(c, "match") {
                        Some(m) => {
                            let pat = self.pattern(doc, c, m)?;
                            self.order += 1;
                            let mut first = true;
                            for alt in pat.alts {
                                let priority = explicit_priority.unwrap_or_else(|| Pattern::default_priority(&alt));
                                self.templates.push(TemplateRule {
                                    pattern: Some(alt),
                                    name: if first { name.clone() } else { None },
                                    mode: mode.clone(),
                                    priority,
                                    precedence: prec,
                                    order: self.order,
                                    params: params.clone(),
                                    body: body.clone(),
                                });
                                first = false;
                            }
                        }
                        None => {
                            if name.is_none() {
                                return Err(cerr("xsl:template must have a match or a name attribute"));
                            }
                            self.order += 1;
                            self.templates.push(TemplateRule {
                                pattern: None,
                                name,
                                mode,
                                priority: 0.0,
                                precedence: prec,
                                order: self.order,
                                params,
                                body,
                            });
                        }
                    }
                }
                "variable" | "param" => {
                    let name = self.qname(doc, c, Self::required(doc, c, "name")?)?;
                    let value = self.var_value(doc, c)?;
                    self.globals.push(Global { name, value, is_param: n.name.local == "param", precedence: prec });
                }
                "key" => {
                    let name = self.qname(doc, c, Self::required(doc, c, "name")?)?;
                    let pattern = self.pattern(doc, c, Self::required(doc, c, "match")?)?;
                    let use_expr = self.xpath(doc, c, Self::required(doc, c, "use")?)?;
                    self.keys.push(KeyDef { name, pattern, use_expr });
                }
                "output" => self.output_decl(doc, c)?,
                "strip-space" | "preserve-space" => {
                    let strip = n.name.local == "strip-space";
                    for t in Self::required(doc, c, "elements")?.split_whitespace() {
                        let test = if t == "*" {
                            None
                        } else if let Some(p) = t.strip_suffix(":*") {
                            Some((doc.lookup_namespace(c, p).ok_or_else(|| cerr(format!("Undeclared prefix {}", p)))?, None))
                        } else {
                            let (u, l) = self.qname(doc, c, t)?;
                            Some((u, Some(l)))
                        };
                        self.space.push(SpaceRule { test, strip, precedence: prec });
                    }
                }
                "decimal-format" => {
                    let name = match doc.attribute(c, "name") {
                        Some(nm) => self.qname(doc, c, nm)?,
                        None => (String::new(), String::new()),
                    };
                    let mut df = DecimalFormat::default();
                    let ch = |a: &str, d: char| doc.attribute(c, a).and_then(|v| v.chars().next()).unwrap_or(d);
                    df.decimal_separator = ch("decimal-separator", '.');
                    df.grouping_separator = ch("grouping-separator", ',');
                    df.minus_sign = ch("minus-sign", '-');
                    df.percent = ch("percent", '%');
                    df.per_mille = ch("per-mille", '\u{2030}');
                    df.zero_digit = ch("zero-digit", '0');
                    df.digit = ch("digit", '#');
                    df.pattern_separator = ch("pattern-separator", ';');
                    if let Some(v) = doc.attribute(c, "infinity") {
                        df.infinity = v.to_string();
                    }
                    if let Some(v) = doc.attribute(c, "NaN") {
                        df.nan = v.to_string();
                    }
                    self.decimal_formats.insert(name, df);
                }
                "attribute-set" => {
                    let name = self.qname(doc, c, Self::required(doc, c, "name")?)?;
                    let uses = self.qnames(doc, c, doc.attribute(c, "use-attribute-sets"))?;
                    let mut instrs = Vec::new();
                    for a in doc.element_children(c) {
                        if Self::is_xsl(doc, a, "attribute") {
                            if let Some(i) = self.instruction(doc, a)? {
                                instrs.push(i);
                            }
                        }
                    }
                    let entry = self.attribute_sets.entry(name).or_insert_with(|| (Vec::new(), Vec::new()));
                    entry.0.extend(uses);
                    entry.1.extend(instrs);
                }
                other => {
                    if XSLT2_INSTRUCTIONS.contains(&other) {
                        return Err(cerr(format!("XSLT 2.0/3.0 declaration xsl:{} is not supported: rust-camel implements XSLT 1.0", other)));
                    }
                    if self.version <= 1.0 {
                        return Err(cerr(format!("Unknown top-level XSLT element xsl:{}", other)));
                    }
                }
            }
        }
        Ok(())
    }

    fn output_decl(&mut self, doc: &Document, c: NodeId) -> Result<(), CamelError> {
        let o = &mut self.output;
        if let Some(m) = doc.attribute(c, "method") {
            o.method = Some(match m.trim() {
                "xml" => Method::Xml,
                "html" => Method::Html,
                "text" => Method::Text,
                "xhtml" => Method::Xml,
                other => return Err(cerr(format!("Unsupported output method: {}", other))),
            });
            self.output_method_set = true;
        }
        if let Some(v) = doc.attribute(c, "indent") {
            o.indent = v.trim() == "yes";
        }
        if let Some(v) = doc.attribute(c, "encoding") {
            o.encoding = v.trim().to_string();
        }
        if let Some(v) = doc.attribute(c, "omit-xml-declaration") {
            o.omit_xml_declaration = v.trim() == "yes";
        }
        if let Some(v) = doc.attribute(c, "standalone") {
            o.standalone = Some(v.trim().to_string());
        }
        if let Some(v) = doc.attribute(c, "doctype-public") {
            o.doctype_public = Some(v.to_string());
        }
        if let Some(v) = doc.attribute(c, "doctype-system") {
            o.doctype_system = Some(v.to_string());
        }
        if let Some(v) = doc.attribute(c, "media-type") {
            o.media_type = Some(v.to_string());
        }
        if let Some(v) = doc.attribute(c, "version") {
            o.version = v.to_string();
        }
        if let Some(v) = doc.attribute(c, "cdata-section-elements") {
            for q in v.split_whitespace() {
                let (u, l) = match q.split_once(':') {
                    Some((p, l)) => (doc.lookup_namespace(c, p).unwrap_or_default(), l.to_string()),
                    None => (doc.lookup_namespace(c, "").unwrap_or_default(), q.to_string()),
                };
                o.cdata_section_elements.push((u, l));
            }
        }
        for ns in XALAN_NS {
            if let Some(v) = doc.attribute_ns(c, ns, "indent-amount") {
                o.indent_amount = v.trim().parse().unwrap_or(o.indent_amount);
            }
        }
        Ok(())
    }
}

/// Resolve an `href` relative to a base directory.
pub(crate) fn resolve_href(href: &str, base: Option<&Path>) -> PathBuf {
    let h = href.trim();
    let h = h.strip_prefix("file://").or_else(|| h.strip_prefix("file:")).or_else(|| h.strip_prefix("classpath:")).unwrap_or(h);
    let p = PathBuf::from(h);
    if p.is_absolute() {
        return p;
    }
    match base {
        Some(b) => b.join(p),
        None => p,
    }
}

impl Stylesheet {
    /// Compile a stylesheet from text. `base_dir` resolves `xsl:include`/`xsl:import` and
    /// `document()` hrefs.
    pub fn compile(text: &str, base_dir: Option<&Path>) -> Result<Stylesheet, CamelError> {
        let doc = Document::parse(text).map_err(|e| cerr(format!("Invalid stylesheet: {}", e)))?;
        Stylesheet::compile_doc(doc, base_dir)
    }

    /// Compile a stylesheet file.
    pub fn compile_file(path: &Path) -> Result<Stylesheet, CamelError> {
        let bytes = std::fs::read(path).map_err(|e| {
            let kind = if e.kind() == std::io::ErrorKind::NotFound { camel_core::error::kinds::FILE_NOT_FOUND } else { camel_core::error::kinds::IO };
            CamelError::new(kind, format!("Cannot load stylesheet {}: {}", path.display(), e))
        })?;
        let doc = Document::parse_bytes(&bytes).map_err(|e| cerr(format!("Invalid stylesheet {}: {}", path.display(), e)))?;
        Stylesheet::compile_doc(doc, path.parent())
    }

    /// Compile a parsed stylesheet document.
    pub fn compile_doc(doc: Document, base_dir: Option<&Path>) -> Result<Stylesheet, CamelError> {
        let mut c = Compiler {
            templates: Vec::new(),
            globals: Vec::new(),
            keys: Vec::new(),
            output: OutputOptions::default(),
            output_method_set: false,
            space: Vec::new(),
            decimal_formats: HashMap::new(),
            attribute_sets: HashMap::new(),
            aliases: Vec::new(),
            next_precedence: 0,
            order: 0,
            version: 1.0,
            loading: Vec::new(),
        };
        c.module(&doc, base_dir, None)?;
        let mut named = HashMap::new();
        for (i, t) in c.templates.iter().enumerate() {
            if let Some(n) = &t.name {
                match named.get(n) {
                    Some(&j) => {
                        let tj: &TemplateRule = &c.templates[j];
                        if t.precedence > tj.precedence {
                            named.insert(n.clone(), i);
                        } else if t.precedence == tj.precedence {
                            return Err(cerr(format!("Duplicate named template '{}'", n.1)));
                        }
                    }
                    None => {
                        named.insert(n.clone(), i);
                    }
                }
            }
        }
        // highest-precedence global wins
        c.globals.sort_by(|a, b| b.precedence.cmp(&a.precedence));
        let mut seen: Vec<Key> = Vec::new();
        c.globals.retain(|g| {
            if seen.contains(&g.name) {
                false
            } else {
                seen.push(g.name.clone());
                true
            }
        });
        let _ = c.output_method_set;
        Ok(Stylesheet {
            templates: c.templates,
            named,
            globals: c.globals,
            keys: c.keys,
            output: c.output,
            space: c.space,
            decimal_formats: c.decimal_formats,
            attribute_sets: c.attribute_sets,
            base_dir: base_dir.map(|b| b.to_path_buf()),
            source: Arc::new(doc),
            version: c.version,
        })
    }

    /// The output options declared by `xsl:output`.
    pub fn output(&self) -> &OutputOptions {
        &self.output
    }

    /// The highest `version` declared by the stylesheet modules.
    pub fn version(&self) -> f64 {
        self.version
    }

    /// Transform a document; `params` override global `xsl:param`s.
    pub fn transform(&self, source: Document, params: &[(String, camel_core::value::Value)]) -> Result<TransformResult, CamelError> {
        exec::run(self, source, params)
    }

    /// Transform XML text and serialize the result with the stylesheet's output options.
    pub fn transform_to_string(&self, xml: &str, params: &[(String, camel_core::value::Value)]) -> Result<String, CamelError> {
        let doc = Document::parse(xml)?;
        Ok(self.transform(doc, params)?.output)
    }
}

/// Convenience: compile a stylesheet and transform XML text.
pub fn transform(xsl: &str, xml: &str) -> Result<String, CamelError> {
    Stylesheet::compile(xsl, None)?.transform_to_string(xml, &[])
}
