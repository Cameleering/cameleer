//! XQuery Update Facility basics: `insert node(s)`, `delete node(s)`, `replace node`,
//! `replace value of node`, `rename node` and `copy ... modify ... return`.
//!
//! Updating expressions add primitives to a pending update list that is applied at the end
//! of the query (or of a `modify` clause), in the order of the specification: renames and
//! value replacements, insertions, node replacements, then deletions.

use std::rc::Rc;

use crate::dom::{Document, NodeId, NodeType, QName};
use crate::xpath::XNode;

use super::ast::{Expr, InsertPos};
use super::eval::{new_root, Focus, Interp, Seq, R};
use super::types::{Atomic, Item, XqError};

/// A pending update primitive.
#[derive(Clone, Debug)]
pub enum Update {
    Insert { target: XNode, pos: InsertPos, content: Vec<XNode> },
    Delete(XNode),
    ReplaceNode { target: XNode, content: Vec<XNode> },
    ReplaceValue { target: XNode, value: String },
    Rename { target: XNode, name: QName },
}

impl Update {
    fn rank(&self) -> u8 {
        match self {
            Update::Rename { .. } | Update::ReplaceValue { .. } => 0,
            Update::Insert { .. } => 1,
            Update::ReplaceNode { .. } => 2,
            Update::Delete(_) => 3,
        }
    }
    fn doc(&self) -> u32 {
        match self {
            Update::Insert { target, .. } | Update::ReplaceNode { target, .. } | Update::ReplaceValue { target, .. } | Update::Rename { target, .. } => target.doc,
            Update::Delete(t) => t.doc,
        }
    }
}

/// Append a copy of `src` (from `from`) under `parent`, never merging text nodes.
fn append_copy(d: &mut Document, from: &Document, src: NodeId, parent: NodeId) {
    match from.nodes[src].ty {
        NodeType::Text => {
            d.append_text_node(parent, &from.nodes[src].value);
        }
        NodeType::Document => {
            for &c in &from.nodes[src].children {
                append_copy(d, from, c, parent);
            }
        }
        _ => {
            d.import_node(from, src, parent);
        }
    }
}

fn detach(d: &mut Document, id: NodeId) {
    if let Some(p) = d.nodes[id].parent {
        d.nodes[p].children.retain(|&c| c != id);
        d.nodes[p].attrs.retain(|&c| c != id);
    }
    d.nodes[id].parent = None;
}

impl Interp<'_> {
    fn target_node(&mut self, e: &Expr, f: &Focus, what: &str) -> R<XNode> {
        let v = self.eval(e, f)?;
        match v.as_slice() {
            [Item::Node(n)] if !n.is_namespace() => Ok(*n),
            [] => Err(XqError::new("XUDY0027", format!("The target of {} is an empty sequence", what))),
            _ => Err(XqError::new("XUTY0008", format!("The target of {} must be a single node", what))),
        }
    }

    /// Content of an insert / replace: nodes (copied later) and atomic values as text.
    fn update_content(&mut self, e: &Expr, f: &Focus) -> R<Vec<XNode>> {
        let v = self.eval(e, f)?;
        let mut out = Vec::new();
        let mut atoms: Vec<String> = Vec::new();
        let flush = |me: &mut Self, atoms: &mut Vec<String>, out: &mut Vec<XNode>| {
            if !atoms.is_empty() {
                let d = new_root(NodeType::Text, QName::default(), atoms.join(" "));
                out.push(XNode::new(me.add_doc(d), 0));
                atoms.clear();
            }
        };
        for it in v {
            match it {
                Item::Node(n) => {
                    flush(self, &mut atoms, &mut out);
                    if self.node_type(n) == Some(NodeType::Document) {
                        let d = self.docs.get(n.doc);
                        out.extend(d.children(n.node_id()).iter().map(|&c| XNode::new(n.doc, c)));
                    } else {
                        out.push(n);
                    }
                }
                other => {
                    for a in self.atomize(&[other]) {
                        atoms.push(a.to_string());
                    }
                }
            }
        }
        flush(self, &mut atoms, &mut out);
        Ok(out)
    }

    pub(crate) fn eval_update(&mut self, e: &Expr, f: &Focus) -> R<Seq> {
        match e {
            Expr::Insert { source, pos, target } => {
                let content = self.update_content(source, f)?;
                let t = self.target_node(target, f, "insert")?;
                let tt = self.node_type(t);
                match pos {
                    InsertPos::Into | InsertPos::First | InsertPos::Last => {
                        if !matches!(tt, Some(NodeType::Element | NodeType::Document)) {
                            return Err(XqError::new("XUTY0005", "The target of 'insert into' must be an element or document node"));
                        }
                    }
                    _ => {
                        let parent = self.docs.get(t.doc).parent(t.node_id());
                        if parent.is_none() {
                            return Err(XqError::new("XUDY0029", "The target of 'insert before/after' has no parent"));
                        }
                    }
                }
                self.pul.push(Update::Insert { target: t, pos: *pos, content });
                Ok(Vec::new())
            }
            Expr::Delete(target) => {
                let v = self.eval(target, f)?;
                for it in v {
                    match it {
                        Item::Node(n) => self.pul.push(Update::Delete(n)),
                        _ => return Err(XqError::new("XUTY0007", "The target of 'delete' must be nodes")),
                    }
                }
                Ok(Vec::new())
            }
            Expr::Replace { value_of, target, with } => {
                let t = self.target_node(target, f, "replace")?;
                if *value_of {
                    let v = self.eval(with, f)?;
                    let value = self.atomize(&v).iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
                    self.pul.push(Update::ReplaceValue { target: t, value });
                } else {
                    if self.docs.get(t.doc).parent(t.node_id()).is_none() {
                        return Err(XqError::new("XUDY0009", "The target of 'replace node' has no parent"));
                    }
                    let content = self.update_content(with, f)?;
                    self.pul.push(Update::ReplaceNode { target: t, content });
                }
                Ok(Vec::new())
            }
            Expr::Rename { target, name } => {
                let t = self.target_node(target, f, "rename")?;
                let v = self.eval(name, f)?;
                let a = self.atomize_opt(&v, "a new name")?.ok_or_else(|| XqError::new("XPTY0004", "The new name of 'rename' is empty"))?;
                let q = match a {
                    Atomic::QName(p, l, u) => QName::ns(p, l, u),
                    other => {
                        let s = other.to_string();
                        let s = s.trim();
                        match s.split_once(':') {
                            Some((p, l)) => {
                                let u = self.module.namespaces.iter().rev().find(|(q, _)| q == p).map(|(_, u)| u.clone()).ok_or_else(|| XqError::new("XUDY0023", format!("Namespace prefix '{}' is not declared", p)))?;
                                QName::ns(p, l, u)
                            }
                            None => {
                                // keep the namespace of the renamed element when unprefixed
                                QName::local(s)
                            }
                        }
                    }
                };
                self.pul.push(Update::Rename { target: t, name: q });
                Ok(Vec::new())
            }
            Expr::Transform { copies, modify, ret } => {
                let base = self.vars.len();
                for (v, e) in copies {
                    let val = self.eval(e, f)?;
                    let n = match val.as_slice() {
                        [Item::Node(n)] => *n,
                        _ => {
                            self.vars.truncate(base);
                            return Err(XqError::new("XUTY0013", format!("The copy source of ${} must be a single node", v.local)));
                        }
                    };
                    let src = self.docs.get(n.doc);
                    let copy = match src.node_type(n.node_id()) {
                        NodeType::Document => {
                            let mut d = (*src).clone();
                            d.base_uri = None;
                            XNode::new(self.add_doc(d), 0)
                        }
                        _ => {
                            let mut d = Document::new();
                            append_copy(&mut d, &src, n.node_id(), 0);
                            let id = d.children(0).first().copied().unwrap_or(0);
                            XNode::new(self.add_doc(d), id)
                        }
                    };
                    self.vars.push((v.clone(), Rc::new(vec![Item::Node(copy)])));
                }
                let outer = std::mem::take(&mut self.pul);
                let r = self.eval(modify, f);
                let pul = std::mem::replace(&mut self.pul, outer);
                let r = r.and_then(|_| self.apply_updates(pul)).and_then(|_| self.eval(ret, f));
                self.vars.truncate(base);
                r
            }
            _ => unreachable!(),
        }
    }

    /// Apply a pending update list.
    pub(crate) fn apply_updates(&mut self, mut pul: Vec<Update>) -> R<()> {
        if pul.is_empty() {
            return Ok(());
        }
        // XUDY0015 / XUDY0016 / XUDY0017: conflicting renames / replacements
        for (i, a) in pul.iter().enumerate() {
            for b in &pul[i + 1..] {
                let clash = match (a, b) {
                    (Update::Rename { target: x, .. }, Update::Rename { target: y, .. }) => x == y,
                    (Update::ReplaceNode { target: x, .. }, Update::ReplaceNode { target: y, .. }) => x == y,
                    (Update::ReplaceValue { target: x, .. }, Update::ReplaceValue { target: y, .. }) => x == y,
                    _ => false,
                };
                if clash {
                    return Err(XqError::new("XUDY0016", "A node is the target of more than one rename / replace"));
                }
            }
        }
        pul.sort_by_key(|u| u.rank());
        let mut docs: Vec<u32> = pul.iter().map(|u| u.doc()).collect();
        docs.sort();
        docs.dedup();
        for di in docs {
            let mut d = (*self.docs.get(di)).clone();
            for u in pul.iter().filter(|u| u.doc() == di) {
                match u {
                    Update::Rename { target, name } => {
                        let id = target.node_id();
                        let mut q = name.clone();
                        if q.prefix.is_empty() && q.uri.is_empty() && d.nodes[id].ty == NodeType::Element {
                            q.uri = d.nodes[id].name.uri.clone();
                            if !q.uri.is_empty() {
                                q.prefix = d.nodes[id].name.prefix.clone();
                            }
                        }
                        if d.nodes[id].ty == NodeType::ProcessingInstruction {
                            q = QName::local(q.local);
                        }
                        d.nodes[id].name = q;
                    }
                    Update::ReplaceValue { target, value } => {
                        let id = target.node_id();
                        match d.nodes[id].ty {
                            NodeType::Element => {
                                let kids = std::mem::take(&mut d.nodes[id].children);
                                for k in kids {
                                    d.nodes[k].parent = None;
                                }
                                if !value.is_empty() {
                                    d.append_text_node(id, value);
                                }
                            }
                            NodeType::Comment if value.contains("--") => return Err(XqError::new("XQDY0072", "A comment must not contain '--'")),
                            _ => d.nodes[id].value = value.clone(),
                        }
                    }
                    Update::Insert { target, pos, content } => {
                        let tid = target.node_id();
                        let (parent, anchor) = match pos {
                            InsertPos::Into | InsertPos::Last | InsertPos::First => (tid, None),
                            InsertPos::Before | InsertPos::After => (d.nodes[tid].parent.ok_or_else(|| XqError::new("XUDY0029", "The insert target has no parent"))?, Some(tid)),
                        };
                        let before = d.nodes[parent].children.len();
                        for n in content {
                            let src = self.docs.get(n.doc);
                            if src.node_type(n.node_id()) == NodeType::Attribute {
                                if d.nodes[parent].ty != NodeType::Element {
                                    return Err(XqError::new("XUTY0022", "Attributes can only be inserted into elements"));
                                }
                                let an = src.name(n.node_id()).clone();
                                d.set_attribute(parent, an, src.value(n.node_id()).to_string());
                                continue;
                            }
                            if Rc::ptr_eq(&src, &self.docs.get(di)) {
                                let snapshot = (*src).clone();
                                append_copy(&mut d, &snapshot, n.node_id(), parent);
                            } else {
                                append_copy(&mut d, &src, n.node_id(), parent);
                            }
                        }
                        let added: Vec<NodeId> = d.nodes[parent].children.drain(before..).collect();
                        let kids = &mut d.nodes[parent].children;
                        let at = match (pos, anchor) {
                            (InsertPos::First, _) => 0,
                            (InsertPos::Before, Some(a)) => kids.iter().position(|&c| c == a).unwrap_or(0),
                            (InsertPos::After, Some(a)) => kids.iter().position(|&c| c == a).map(|i| i + 1).unwrap_or(kids.len()),
                            _ => kids.len(),
                        };
                        for (k, id) in added.into_iter().enumerate() {
                            kids.insert(at + k, id);
                        }
                    }
                    Update::ReplaceNode { target, content } => {
                        let tid = target.node_id();
                        let parent = d.nodes[tid].parent.ok_or_else(|| XqError::new("XUDY0009", "The replace target has no parent"))?;
                        if d.nodes[tid].ty == NodeType::Attribute {
                            detach(&mut d, tid);
                            for n in content {
                                let src = self.docs.get(n.doc);
                                if src.node_type(n.node_id()) != NodeType::Attribute {
                                    return Err(XqError::new("XUTY0011", "An attribute can only be replaced by attributes"));
                                }
                                d.set_attribute(parent, src.name(n.node_id()).clone(), src.value(n.node_id()).to_string());
                            }
                            continue;
                        }
                        let before = d.nodes[parent].children.len();
                        for n in content {
                            let src = self.docs.get(n.doc);
                            let snapshot;
                            let from: &Document = if Rc::ptr_eq(&src, &self.docs.get(di)) {
                                snapshot = (*src).clone();
                                &snapshot
                            } else {
                                &src
                            };
                            append_copy(&mut d, from, n.node_id(), parent);
                        }
                        let added: Vec<NodeId> = d.nodes[parent].children.drain(before..).collect();
                        let kids = &mut d.nodes[parent].children;
                        let at = kids.iter().position(|&c| c == tid).unwrap_or(kids.len());
                        for (k, id) in added.into_iter().enumerate() {
                            kids.insert(at + k, id);
                        }
                        detach(&mut d, tid);
                    }
                    Update::Delete(t) => {
                        if t.is_namespace() {
                            continue;
                        }
                        detach(&mut d, t.node_id());
                    }
                }
            }
            self.docs.replace(di, d);
        }
        Ok(())
    }
}
