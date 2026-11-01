//! Arena-based, namespace-aware DOM following the XPath 1.0 data model.
//!
//! A [`Document`] is an arena of [`Node`]s. Node `0` is the document (root) node. An arena
//! may hold several trees (each starting with a `Document` node, see [`Document::new_tree`]),
//! which XSLT uses for result tree fragments. Nodes are appended in document order
//! (an element, then its attributes, then its children), so the arena index gives the
//! document order of nodes.
//!
//! Namespace declarations are not attributes: they are kept in [`Node::ns_decls`] of the
//! element that declares them and [`Document::in_scope_namespaces`] computes the
//! namespace nodes of an element.

use std::collections::HashMap;

/// Index of a node in a [`Document`] arena.
pub type NodeId = usize;

/// The XML namespace (`xml:` prefix).
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
/// The namespace of namespace declarations (`xmlns:`).
pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

/// Node types of the XPath data model (namespace nodes are virtual, see
/// [`Document::in_scope_namespaces`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    Document,
    Element,
    Attribute,
    Text,
    Comment,
    ProcessingInstruction,
}

/// An expanded name: prefix (as written), local part and namespace URI (empty for none).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct QName {
    pub prefix: String,
    pub local: String,
    pub uri: String,
}

impl QName {
    /// A name without namespace.
    pub fn local(local: impl Into<String>) -> QName {
        QName { prefix: String::new(), local: local.into(), uri: String::new() }
    }

    /// A namespaced name.
    pub fn ns(prefix: impl Into<String>, local: impl Into<String>, uri: impl Into<String>) -> QName {
        QName { prefix: prefix.into(), local: local.into(), uri: uri.into() }
    }

    /// The qualified name `prefix:local` (or `local`).
    pub fn qualified(&self) -> String {
        if self.prefix.is_empty() {
            self.local.clone()
        } else {
            format!("{}:{}", self.prefix, self.local)
        }
    }

    /// Whether both names have the same local part and namespace URI.
    pub fn same_expanded(&self, other: &QName) -> bool {
        self.local == other.local && self.uri == other.uri
    }
}

/// A node of the arena.
#[derive(Clone, Debug)]
pub struct Node {
    pub ty: NodeType,
    pub parent: Option<NodeId>,
    /// Child nodes (document and element nodes).
    pub children: Vec<NodeId>,
    /// Attribute nodes (element nodes).
    pub attrs: Vec<NodeId>,
    /// Namespace declarations made on this element (`("", uri)` for the default namespace;
    /// `("", "")` undeclares it).
    pub ns_decls: Vec<(String, String)>,
    /// Element / attribute name; for processing instructions `local` is the target.
    pub name: QName,
    /// Text, comment, PI data or attribute value.
    pub value: String,
    /// Source line (1-based, 0 when unknown).
    pub line: u32,
    /// Source column (1-based, 0 when unknown).
    pub col: u32,
}

impl Node {
    fn new(ty: NodeType, parent: Option<NodeId>) -> Node {
        Node {
            ty,
            parent,
            children: Vec::new(),
            attrs: Vec::new(),
            ns_decls: Vec::new(),
            name: QName::default(),
            value: String::new(),
            line: 0,
            col: 0,
        }
    }
}

/// An XML document (or a forest of trees, see the module documentation).
#[derive(Clone, Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
    /// ID values (attributes declared with type ID in the DTD, and `xml:id`) → element.
    pub ids: HashMap<String, NodeId>,
    /// Unparsed entities declared in the DTD (name → system id).
    pub unparsed_entities: HashMap<String, String>,
    /// Base URI (file path) the document was loaded from.
    pub base_uri: Option<String>,
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

impl Document {
    /// An empty document (only the document node).
    pub fn new() -> Document {
        Document { nodes: vec![Node::new(NodeType::Document, None)], ids: HashMap::new(), unparsed_entities: HashMap::new(), base_uri: None }
    }

    /// Parse XML text (see [`crate::parser`]).
    pub fn parse(text: &str) -> Result<Document, crate::parser::XmlError> {
        crate::parser::parse(text)
    }

    /// Parse XML bytes, honouring the BOM / encoding declaration.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Document, crate::parser::XmlError> {
        crate::parser::parse_bytes(bytes)
    }

    /// Start a new tree in this arena; returns its document node.
    pub fn new_tree(&mut self) -> NodeId {
        self.nodes.push(Node::new(NodeType::Document, None));
        self.nodes.len() - 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn node_type(&self, id: NodeId) -> NodeType {
        self.nodes[id].ty
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        self.nodes[id].ty == NodeType::Element
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id].parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    pub fn attributes(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].attrs
    }

    pub fn name(&self, id: NodeId) -> &QName {
        &self.nodes[id].name
    }

    pub fn local_name(&self, id: NodeId) -> &str {
        &self.nodes[id].name.local
    }

    pub fn namespace_uri(&self, id: NodeId) -> &str {
        &self.nodes[id].name.uri
    }

    pub fn qualified_name(&self, id: NodeId) -> String {
        self.nodes[id].name.qualified()
    }

    pub fn value(&self, id: NodeId) -> &str {
        &self.nodes[id].value
    }

    /// The document node of the tree containing `id`.
    pub fn tree_root(&self, mut id: NodeId) -> NodeId {
        while let Some(p) = self.nodes[id].parent {
            id = p;
        }
        id
    }

    /// The document element (first element child of node 0).
    pub fn document_element(&self) -> Option<NodeId> {
        self.document_element_of(0)
    }

    /// The first element child of a document node.
    pub fn document_element_of(&self, doc: NodeId) -> Option<NodeId> {
        self.nodes[doc].children.iter().copied().find(|&c| self.nodes[c].ty == NodeType::Element)
    }

    /// Element children of a node.
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.nodes[id].children.iter().copied().filter(|&c| self.nodes[c].ty == NodeType::Element).collect()
    }

    /// Element children with the given local name and namespace URI.
    pub fn child_elements_named<'a>(&'a self, id: NodeId, uri: &'a str, local: &'a str) -> impl Iterator<Item = NodeId> + 'a {
        self.nodes[id].children.iter().copied().filter(move |&c| {
            let n = &self.nodes[c];
            n.ty == NodeType::Element && n.name.local == local && n.name.uri == uri
        })
    }

    /// Attribute value by namespace URI and local name.
    pub fn attribute_ns(&self, el: NodeId, uri: &str, local: &str) -> Option<&str> {
        self.nodes[el].attrs.iter().map(|&a| &self.nodes[a]).find(|a| a.name.local == local && a.name.uri == uri).map(|a| a.value.as_str())
    }

    /// Value of an attribute without namespace.
    pub fn attribute(&self, el: NodeId, local: &str) -> Option<&str> {
        self.attribute_ns(el, "", local)
    }

    /// Value of an attribute by qualified name as written (`xml:lang`, `a`).
    pub fn attribute_by_qname(&self, el: NodeId, qname: &str) -> Option<&str> {
        self.nodes[el].attrs.iter().map(|&a| &self.nodes[a]).find(|a| a.name.qualified() == qname).map(|a| a.value.as_str())
    }

    /// The string value of a node (XPath `string()`).
    pub fn string_value(&self, id: NodeId) -> String {
        match self.nodes[id].ty {
            NodeType::Document | NodeType::Element => {
                let mut s = String::new();
                self.collect_text(id, &mut s);
                s
            }
            _ => self.nodes[id].value.clone(),
        }
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        // iterative to support very deep trees
        let mut stack: Vec<NodeId> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            let node = &self.nodes[n];
            match node.ty {
                NodeType::Text => out.push_str(&node.value),
                NodeType::Element => stack.extend(node.children.iter().rev().copied()),
                _ => {}
            }
        }
    }

    /// In-scope namespaces of an element as `(prefix, uri)` pairs, sorted by prefix
    /// (the default namespace has the empty prefix). Always includes `xml`.
    pub fn in_scope_namespaces(&self, el: NodeId) -> Vec<(String, String)> {
        let mut map: Vec<(String, String)> = Vec::new();
        let mut cur = Some(el);
        let mut seen: Vec<String> = Vec::new();
        while let Some(c) = cur {
            if self.nodes[c].ty != NodeType::Element {
                break;
            }
            for (p, u) in &self.nodes[c].ns_decls {
                if !seen.contains(p) {
                    seen.push(p.clone());
                    if !u.is_empty() {
                        map.push((p.clone(), u.clone()));
                    }
                }
            }
            cur = self.nodes[c].parent;
        }
        if !seen.iter().any(|p| p == "xml") {
            map.push(("xml".into(), XML_NS.into()));
        }
        map.sort();
        map
    }

    /// Resolve a prefix (empty for the default namespace) in the scope of an element.
    pub fn lookup_namespace(&self, el: NodeId, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(XML_NS.into());
        }
        let mut cur = Some(el);
        while let Some(c) = cur {
            if let Some((_, u)) = self.nodes[c].ns_decls.iter().find(|(p, _)| p == prefix) {
                return if u.is_empty() { None } else { Some(u.clone()) };
            }
            cur = self.nodes[c].parent;
        }
        None
    }

    /// The effective `xml:lang` of a node.
    pub fn lang(&self, id: NodeId) -> Option<String> {
        let mut cur = Some(id);
        while let Some(c) = cur {
            if self.nodes[c].ty == NodeType::Element {
                if let Some(v) = self.attribute_ns(c, XML_NS, "lang") {
                    return Some(v.to_string());
                }
            }
            cur = self.nodes[c].parent;
        }
        None
    }

    // ---- construction ----

    fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Append an element under `parent`.
    pub fn append_element(&mut self, parent: NodeId, name: QName) -> NodeId {
        let mut n = Node::new(NodeType::Element, Some(parent));
        n.name = name;
        let id = self.push(n);
        self.nodes[parent].children.push(id);
        id
    }

    /// Add (or replace) an attribute of an element.
    pub fn set_attribute(&mut self, el: NodeId, name: QName, value: impl Into<String>) -> NodeId {
        let value = value.into();
        if let Some(&a) = self.nodes[el].attrs.iter().find(|&&a| self.nodes[a].name.same_expanded(&name)) {
            self.nodes[a].value = value;
            self.nodes[a].name.prefix = name.prefix;
            return a;
        }
        let mut n = Node::new(NodeType::Attribute, Some(el));
        n.name = name;
        n.value = value;
        let id = self.push(n);
        self.nodes[el].attrs.push(id);
        id
    }

    /// Declare a namespace on an element (replaces an existing declaration of the prefix).
    pub fn declare_namespace(&mut self, el: NodeId, prefix: &str, uri: &str) {
        let decls = &mut self.nodes[el].ns_decls;
        if let Some(d) = decls.iter_mut().find(|(p, _)| p == prefix) {
            d.1 = uri.to_string();
        } else {
            decls.push((prefix.to_string(), uri.to_string()));
        }
    }

    /// Append text under `parent`, merging with a preceding text node. Empty text is ignored.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(&last) = self.nodes[parent].children.last() {
            // only merge when no node was created after the last text node (document order)
            if self.nodes[last].ty == NodeType::Text && last == self.nodes.len() - 1 {
                self.nodes[last].value.push_str(text);
                return;
            }
        }
        let mut n = Node::new(NodeType::Text, Some(parent));
        n.value = text.to_string();
        let id = self.push(n);
        self.nodes[parent].children.push(id);
    }

    /// Append a text node (never merged with a preceding text node; may be empty).
    pub fn append_text_node(&mut self, parent: NodeId, text: &str) -> NodeId {
        let mut n = Node::new(NodeType::Text, Some(parent));
        n.value = text.to_string();
        let id = self.push(n);
        self.nodes[parent].children.push(id);
        id
    }

    /// Append a comment.
    pub fn append_comment(&mut self, parent: NodeId, text: &str) -> NodeId {
        let mut n = Node::new(NodeType::Comment, Some(parent));
        n.value = text.to_string();
        let id = self.push(n);
        self.nodes[parent].children.push(id);
        id
    }

    /// Append a processing instruction.
    pub fn append_pi(&mut self, parent: NodeId, target: &str, data: &str) -> NodeId {
        let mut n = Node::new(NodeType::ProcessingInstruction, Some(parent));
        n.name = QName::local(target);
        n.value = data.to_string();
        let id = self.push(n);
        self.nodes[parent].children.push(id);
        id
    }

    /// Deep-copy node `src` of `from` under `parent` of this document (attributes of
    /// a copied attribute node are added to `parent`). Namespace nodes in scope of a copied
    /// element are declared on the copy. Returns the new node (None for a document node,
    /// whose children are copied).
    pub fn import_node(&mut self, from: &Document, src: NodeId, parent: NodeId) -> Option<NodeId> {
        let n = &from.nodes[src];
        match n.ty {
            NodeType::Document => {
                for &c in &n.children {
                    self.import_node(from, c, parent);
                }
                None
            }
            NodeType::Attribute => {
                if self.nodes[parent].ty == NodeType::Element {
                    Some(self.set_attribute(parent, n.name.clone(), n.value.clone()))
                } else {
                    None
                }
            }
            NodeType::Text => {
                self.append_text(parent, &n.value);
                None
            }
            NodeType::Comment => Some(self.append_comment(parent, &n.value)),
            NodeType::ProcessingInstruction => Some(self.append_pi(parent, &n.name.local, &n.value)),
            NodeType::Element => {
                let el = self.append_element(parent, n.name.clone());
                self.nodes[el].line = n.line;
                self.nodes[el].col = n.col;
                for (p, u) in from.in_scope_namespaces(src) {
                    if p != "xml" {
                        self.declare_namespace(el, &p, &u);
                    }
                }
                self.copy_children(from, src, el);
                Some(el)
            }
        }
    }

    fn copy_children(&mut self, from: &Document, src: NodeId, el: NodeId) {
        for &a in &from.nodes[src].attrs {
            let an = &from.nodes[a];
            self.set_attribute(el, an.name.clone(), an.value.clone());
        }
        for &c in &from.nodes[src].children {
            let cn = &from.nodes[c];
            if cn.ty == NodeType::Element {
                let ce = self.append_element(el, cn.name.clone());
                self.nodes[ce].line = cn.line;
                self.nodes[ce].col = cn.col;
                for (p, u) in &cn.ns_decls {
                    self.declare_namespace(ce, p, u);
                }
                self.copy_children(from, c, ce);
            } else {
                self.import_node(from, c, el);
            }
        }
    }

    /// Serialize a node as XML (no XML declaration), see [`crate::serialize`].
    pub fn to_xml(&self, id: NodeId) -> String {
        crate::serialize::node_to_xml(self, id)
    }

    /// Serialize the whole document (node 0) as XML without declaration.
    pub fn to_xml_string(&self) -> String {
        crate::serialize::node_to_xml(self, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_query() {
        let mut d = Document::new();
        let r = d.append_element(0, QName::local("a"));
        d.declare_namespace(r, "p", "urn:p");
        d.set_attribute(r, QName::local("x"), "1");
        let b = d.append_element(r, QName::ns("p", "b", "urn:p"));
        d.append_text(b, "hi");
        d.append_text(b, " there");
        assert_eq!(d.string_value(r), "hi there");
        assert_eq!(d.attribute(r, "x"), Some("1"));
        assert_eq!(d.lookup_namespace(b, "p").as_deref(), Some("urn:p"));
        assert_eq!(d.in_scope_namespaces(b).len(), 2);
        assert_eq!(d.to_xml(r), "<a xmlns:p=\"urn:p\" x=\"1\"><p:b>hi there</p:b></a>");
    }
}
