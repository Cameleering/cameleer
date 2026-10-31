//! XML canonicalization: Canonical XML 1.0 (inclusive) and Exclusive XML Canonicalization
//! 1.0, without comments (and the `#WithComments` variants), for whole documents or
//! element subtrees, optionally excluding one subtree (the enveloped-signature transform).

use std::collections::BTreeMap;

use crate::dom::{Document, NodeId, NodeType, XML_NS};

/// Canonical XML 1.0.
pub const C14N: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315";
/// Canonical XML 1.0 with comments.
pub const C14N_WITH_COMMENTS: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315#WithComments";
/// Exclusive XML Canonicalization 1.0.
pub const EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
/// Exclusive XML Canonicalization 1.0 with comments.
pub const EXC_C14N_WITH_COMMENTS: &str = "http://www.w3.org/2001/10/xml-exc-c14n#WithComments";

/// Canonicalization options.
#[derive(Clone, Debug, Default)]
pub struct C14nOptions {
    pub exclusive: bool,
    pub with_comments: bool,
    /// Exclusive c14n `InclusiveNamespaces PrefixList` (`#default` for the default namespace).
    pub inclusive_prefixes: Vec<String>,
    /// A subtree left out of the output.
    pub exclude: Option<NodeId>,
}

impl C14nOptions {
    /// Options for an algorithm URI (None when unsupported).
    pub fn for_algorithm(uri: &str) -> Option<C14nOptions> {
        Some(match uri {
            C14N => C14nOptions::default(),
            C14N_WITH_COMMENTS => C14nOptions { with_comments: true, ..Default::default() },
            EXC_C14N => C14nOptions { exclusive: true, ..Default::default() },
            EXC_C14N_WITH_COMMENTS => C14nOptions { exclusive: true, with_comments: true, ..Default::default() },
            _ => return None,
        })
    }
}

fn esc_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn esc_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

/// Canonicalize a node: a document node (whole document) or an element (its subtree, with
/// the namespace context of its ancestors).
pub fn canonicalize(doc: &Document, node: NodeId, opts: &C14nOptions) -> String {
    let mut out = String::new();
    if doc.node_type(node) == NodeType::Document {
        let kids = doc.children(node);
        let root_pos = kids.iter().position(|&c| doc.node_type(c) == NodeType::Element);
        for (i, &c) in kids.iter().enumerate() {
            let ty = doc.node_type(c);
            let keep = match ty {
                NodeType::Element => true,
                NodeType::ProcessingInstruction => true,
                NodeType::Comment => opts.with_comments,
                _ => false,
            };
            if !keep || Some(c) == opts.exclude {
                continue;
            }
            let after_root = root_pos.map(|r| i > r).unwrap_or(false);
            if ty != NodeType::Element && after_root {
                out.push('\n');
            }
            node_out(doc, c, opts, &BTreeMap::new(), &mut out);
            if ty != NodeType::Element && !after_root {
                out.push('\n');
            }
        }
    } else {
        // namespaces rendered "by ancestors" are none: the apex renders what it needs
        node_out(doc, node, opts, &BTreeMap::new(), &mut out);
    }
    out
}

fn node_out(doc: &Document, n: NodeId, opts: &C14nOptions, rendered: &BTreeMap<String, String>, out: &mut String) {
    if Some(n) == opts.exclude {
        return;
    }
    let node = doc.node(n);
    match node.ty {
        NodeType::Text => esc_text(&node.value, out),
        NodeType::Comment => {
            if opts.with_comments {
                out.push_str("<!--");
                out.push_str(&node.value);
                out.push_str("-->");
            }
        }
        NodeType::ProcessingInstruction => {
            if node.name.local == crate::serialize::RAW_TEXT_TARGET {
                out.push_str(&node.value);
                return;
            }
            out.push_str("<?");
            out.push_str(&node.name.local);
            if !node.value.is_empty() {
                out.push(' ');
                out.push_str(&node.value);
            }
            out.push_str("?>");
        }
        NodeType::Element => {
            // namespace nodes to render
            let in_scope: BTreeMap<String, String> = doc.in_scope_namespaces(n).into_iter().filter(|(p, _)| p != "xml").collect();
            let mut decls: BTreeMap<String, String> = BTreeMap::new();
            let mut now = rendered.clone();
            let candidates: Vec<String> = if opts.exclusive {
                let mut used: Vec<String> = vec![node.name.prefix.clone()];
                for &a in &node.attrs {
                    let an = doc.node(a);
                    if !an.name.prefix.is_empty() && an.name.uri != XML_NS {
                        used.push(an.name.prefix.clone());
                    }
                }
                for p in &opts.inclusive_prefixes {
                    used.push(if p == "#default" { String::new() } else { p.clone() });
                }
                used.sort();
                used.dedup();
                used
            } else {
                let mut all: Vec<String> = in_scope.keys().cloned().collect();
                all.push(String::new());
                all.sort();
                all.dedup();
                all
            };
            for p in candidates {
                let uri = if p == node.name.prefix && opts.exclusive { node.name.uri.clone() } else { in_scope.get(&p).cloned().unwrap_or_default() };
                let prev = rendered.get(&p).cloned().unwrap_or_default();
                if p.is_empty() {
                    if uri != prev {
                        decls.insert(p.clone(), uri.clone());
                        now.insert(p.clone(), uri.clone());
                    }
                } else if !uri.is_empty() && rendered.get(&p) != Some(&uri) {
                    decls.insert(p.clone(), uri.clone());
                    now.insert(p.clone(), uri.clone());
                }
            }
            let q = node.name.qualified();
            out.push('<');
            out.push_str(&q);
            for (p, u) in &decls {
                if p.is_empty() {
                    out.push_str(" xmlns=\"");
                } else {
                    out.push_str(" xmlns:");
                    out.push_str(p);
                    out.push_str("=\"");
                }
                esc_attr(u, out);
                out.push('"');
            }
            let mut attrs: Vec<(String, String, String, String)> = node
                .attrs
                .iter()
                .map(|&a| {
                    let an = doc.node(a);
                    (an.name.uri.clone(), an.name.local.clone(), an.name.qualified(), an.value.clone())
                })
                .collect();
            attrs.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
            for (_, _, qn, v) in &attrs {
                out.push(' ');
                out.push_str(qn);
                out.push_str("=\"");
                esc_attr(v, out);
                out.push('"');
            }
            out.push('>');
            for &c in &node.children {
                node_out(doc, c, opts, &now, out);
            }
            out.push_str("</");
            out.push_str(&q);
            out.push('>');
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms() {
        let d = Document::parse("<?xml version='1.0'?>\n<!-- c --><r xmlns:b='urn:b' xmlns:a='urn:a' z='1' a:y=\"2\" b:x='&quot;'><e/><a:f>t&gt;\r</a:f></r>").unwrap();
        assert_eq!(
            canonicalize(&d, 0, &C14nOptions::default()),
            "<r xmlns:a=\"urn:a\" xmlns:b=\"urn:b\" z=\"1\" a:y=\"2\" b:x=\"&quot;\"><e></e><a:f>t&gt;\n</a:f></r>"
        );
        let with = C14nOptions { with_comments: true, ..Default::default() };
        assert!(canonicalize(&d, 0, &with).starts_with("<!-- c -->\n<r"));
        let f = d.element_children(d.document_element().unwrap())[1];
        let exc = C14nOptions { exclusive: true, ..Default::default() };
        assert_eq!(canonicalize(&d, f, &exc), "<a:f xmlns:a=\"urn:a\">t&gt;\n</a:f>");
        assert_eq!(canonicalize(&d, f, &C14nOptions::default()), "<a:f xmlns:a=\"urn:a\" xmlns:b=\"urn:b\">t&gt;\n</a:f>");
    }
}
