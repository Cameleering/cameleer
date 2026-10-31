//! POJO-mode mapping between [`Value`]s and SOAP body elements, driven by the WSDL.
//!
//! A request or response is described by a [`Shape`]:
//!
//! * document/literal *wrapped* (the single part's element is named after the operation, or
//!   the response element): the parameters are the children of the wrapper element;
//! * rpc/literal: the wrapper is `{soap:body namespace}operation` (`...Response`) and the
//!   parameters are the parts (unqualified accessors);
//! * document/literal *bare*: every part is a body element and a parameter;
//! * without WSDL: a structural wrapper `{operationNamespace}operation` whose parameters are
//!   named `arg0`, `arg1`... (request) and `return` (response), like JAX-WS defaults.
//!
//! Encoding (route body → XML): an `Array` gives the parameters by position, an `Object`
//! by name (an object matching no parameter name is the single complex parameter), and a
//! scalar is the single parameter. Decoding (XML → body): request parameters become an
//! `Array` in declaration order (Camel's `MessageContentsList`); a response with one
//! parameter becomes that value, with several an `Object` keyed by parameter name.

use camel_core::error::{CamelError, Result};
use camel_core::value::{ObjectMap, Value};
use camel_xml::dom::Document;

use crate::support::{key_of, Key};
use crate::wsdl::{MessageInfo, OperationInfo, Style, Wsdl};
use crate::xsdmodel::{BindOptions, SchemaSet, TypeInfo, XmlWriter};

/// A parameter of a message.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: Key,
    pub info: TypeInfo,
    pub nillable: bool,
    pub repeated: bool,
}

/// The XML shape of a request or response.
#[derive(Clone, Debug)]
pub enum Shape {
    /// Wrapper element with typed parameters (`None` = structural, JAX-WS default names).
    Wrapper { element: Key, params: Option<Vec<Param>>, default_name: String },
    /// Each parameter is a body element.
    Bare { params: Vec<Param> },
}

impl Shape {
    /// The shape of an operation's input (`response=false`) or output.
    pub fn of(wsdl: &Wsdl, op: &OperationInfo, response: bool) -> Option<Shape> {
        let msg: &MessageInfo = if response { op.output.as_ref()? } else { &op.input };
        let s = &wsdl.schema;
        let param = |p: &crate::wsdl::PartInfo| Param { name: p.element.clone(), info: wsdl.part_type(p), nillable: p.decl.nillable, repeated: p.decl.repeated() };
        Some(match op.style {
            Style::Rpc => Shape::Wrapper {
                element: if response { op.response_element()? } else { op.request_element()? },
                params: Some(msg.body_parts.iter().map(param).collect()),
                default_name: String::new(),
            },
            Style::Document => {
                let wrapped = msg.body_parts.len() == 1
                    && (if response { msg.body_parts[0].element.1 == format!("{}Response", op.name) || op.wrapped() } else { op.wrapped() })
                    && matches!(wsdl.part_type(&msg.body_parts[0]), TypeInfo::Complex { .. });
                if wrapped {
                    let p = &msg.body_parts[0];
                    let params = match wsdl.part_type(p) {
                        TypeInfo::Complex { children, .. } => children
                            .iter()
                            .map(|d| {
                                let d = s.resolve_element(d);
                                Param { name: d.name.clone(), info: s.element_info(&d), nillable: d.nillable, repeated: d.repeated() }
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    Shape::Wrapper { element: p.element.clone(), params: Some(params), default_name: String::new() }
                } else {
                    Shape::Bare { params: msg.body_parts.iter().map(param).collect() }
                }
            }
        })
    }

    /// A structural shape (no WSDL).
    pub fn structural(ns: &str, op: &str, response: bool) -> Shape {
        Shape::Wrapper {
            element: (ns.to_string(), if response { format!("{}Response", op) } else { op.to_string() }),
            params: None,
            default_name: if response { "return".into() } else { "arg".into() },
        }
    }
}

fn assign<'a>(params: &[Param], body: &'a Value) -> Vec<Option<&'a Value>> {
    let mut out: Vec<Option<&Value>> = vec![None; params.len()];
    match body {
        Value::Array(items) if !(params.len() == 1 && params[0].repeated) => {
            for (i, v) in items.iter().enumerate().take(params.len()) {
                out[i] = Some(v);
            }
        }
        Value::Object(m) if params.iter().any(|p| m.contains_key(&p.name.1)) => {
            for (i, p) in params.iter().enumerate() {
                out[i] = m.get(&p.name.1);
            }
        }
        Value::Null if params.is_empty() => {}
        other => {
            if !params.is_empty() {
                out[0] = Some(other);
            }
        }
    }
    out
}

/// Encode a body value to body element fragments.
pub fn encode(schema: &SchemaSet, shape: &Shape, body: &Value, o: &BindOptions) -> Result<Vec<String>> {
    match shape {
        Shape::Wrapper { element, params, default_name } => {
            let mut w = XmlWriter::new(&o.prefixes);
            let root = w.element(0, element);
            match params {
                Some(ps) => {
                    for (p, v) in ps.iter().zip(assign(ps, body)) {
                        if let Some(v) = v {
                            schema.write_value(&mut w, root, &p.name, v, &p.info, p.nillable, o)?;
                        } else if p.nillable && !p.repeated {
                            schema.write_value(&mut w, root, &p.name, &Value::Null, &p.info, true, o)?;
                        }
                    }
                }
                None => {
                    let items: Vec<(String, &Value)> = match body {
                        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (format!("{}{}", default_name, i), v)).collect(),
                        Value::Object(m) if default_name != "return" => m.iter().map(|(k, v)| (k.clone(), v)).collect(),
                        Value::Null => Vec::new(),
                        other => vec![(if default_name == "return" { "return".to_string() } else { format!("{}0", default_name) }, other)],
                    };
                    for (n, v) in items {
                        schema.write_value(&mut w, root, &(String::new(), n), v, &TypeInfo::Unknown, true, o)?;
                    }
                }
            }
            Ok(vec![w.doc.to_xml_string()])
        }
        Shape::Bare { params } => {
            let mut out = Vec::new();
            for (p, v) in params.iter().zip(assign(params, body)) {
                let mut w = XmlWriter::new(&o.prefixes);
                let v = v.cloned().unwrap_or(Value::Null);
                schema.write_value(&mut w, 0, &p.name, &v, &p.info, true, o)?;
                out.push(w.doc.to_xml_string());
            }
            Ok(out)
        }
    }
}

/// Decode body element fragments; `response` selects the result convention (see the module
/// documentation).
pub fn decode(schema: &SchemaSet, shape: &Shape, body: &[String], response: bool, o: &BindOptions) -> Result<Value> {
    let parse = |x: &String| Document::parse(x).map_err(|e| CamelError::new(camel_xml::SAX_PARSE_EXCEPTION, e.to_string()));
    let mut named: Vec<(String, Value)> = Vec::new();
    match shape {
        Shape::Wrapper { params, .. } => {
            let Some(first) = body.first() else { return Ok(Value::Null) };
            let doc = parse(first)?;
            let root = doc.document_element().unwrap_or(0);
            match params {
                Some(ps) => {
                    let mut values: Vec<Option<Value>> = vec![None; ps.len()];
                    let mut extra = ObjectMap::new();
                    for c in doc.element_children(root) {
                        let k = key_of(&doc, c);
                        match ps.iter().position(|p| p.name.1 == k.1) {
                            Some(i) => {
                                let v = schema.element_to_value(&doc, c, &ps[i].info, o);
                                if ps[i].repeated {
                                    match &mut values[i] {
                                        Some(Value::Array(a)) => a.push(v),
                                        _ => values[i] = Some(Value::Array(vec![v])),
                                    }
                                } else {
                                    values[i] = Some(v);
                                }
                            }
                            None => {
                                extra.insert(k.1.clone(), schema.element_to_value(&doc, c, &TypeInfo::Unknown, o));
                            }
                        }
                    }
                    for (p, v) in ps.iter().zip(values) {
                        named.push((p.name.1.clone(), v.unwrap_or(if p.repeated { Value::Array(Vec::new()) } else { Value::Null })));
                    }
                    named.extend(extra);
                }
                None => {
                    for c in doc.element_children(root) {
                        named.push((doc.local_name(c).to_string(), schema.element_to_value(&doc, c, &TypeInfo::Unknown, o)));
                    }
                }
            }
        }
        Shape::Bare { params } => {
            for (i, x) in body.iter().enumerate() {
                let doc = parse(x)?;
                let root = doc.document_element().unwrap_or(0);
                let k = key_of(&doc, root);
                let p = params.iter().find(|p| p.name.1 == k.1).or_else(|| params.get(i));
                let info = p.map(|p| p.info.clone()).unwrap_or(TypeInfo::Unknown);
                named.push((k.1.clone(), schema.element_to_value(&doc, root, &info, o)));
            }
        }
    }
    if response {
        Ok(match named.len() {
            0 => Value::Null,
            1 => named.pop().map(|(_, v)| v).unwrap_or(Value::Null),
            _ => {
                let mut m = ObjectMap::new();
                for (k, v) in named {
                    m.insert(k, v);
                }
                Value::Object(m)
            }
        })
    } else {
        Ok(Value::Array(named.into_iter().map(|(_, v)| v).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{CALCULATOR_WSDL, CALC_NS, STOCK_RPC_WSDL};

    #[test]
    fn wrapped_and_rpc() {
        let w = Wsdl::parse(CALCULATOR_WSDL, None).unwrap();
        let op = w.operation(None, None, "Add").unwrap();
        let o = BindOptions { prefixes: vec![(CALC_NS.to_string(), "tns".to_string())], ..Default::default() };
        let req = Shape::of(&w, &op, false).unwrap();
        let x = encode(&w.schema, &req, &Value::Array(vec![Value::Int(2), Value::Int(3)]), &o).unwrap();
        assert_eq!(x[0], format!("<tns:Add xmlns:tns=\"{}\"><tns:a>2</tns:a><tns:b>3</tns:b></tns:Add>", CALC_NS));
        let by_name = encode(&w.schema, &req, &Value::object([("b", 3), ("a", 2)]), &o).unwrap();
        assert_eq!(by_name, x);
        assert_eq!(decode(&w.schema, &req, &x, false, &o).unwrap(), Value::Array(vec![Value::Int(2), Value::Int(3)]));
        let resp = Shape::of(&w, &op, true).unwrap();
        let rx = encode(&w.schema, &resp, &Value::Int(5), &o).unwrap();
        assert_eq!(decode(&w.schema, &resp, &rx, true, &o).unwrap(), Value::Int(5));

        let w = Wsdl::parse(STOCK_RPC_WSDL, None).unwrap();
        let op = w.operation(None, None, "GetLastTradePrice").unwrap();
        let req = Shape::of(&w, &op, false).unwrap();
        let x = encode(&w.schema, &req, &Value::from("ACME"), &BindOptions::default()).unwrap();
        assert_eq!(x[0], "<ns1:GetLastTradePrice xmlns:ns1=\"urn:example:stock\"><tickerSymbol>ACME</tickerSymbol></ns1:GetLastTradePrice>");
        let resp = Shape::of(&w, &op, true).unwrap();
        let r = decode(&w.schema, &resp, &["<m:GetLastTradePriceResponse xmlns:m=\"urn:example:stock\"><result>34.5</result></m:GetLastTradePriceResponse>".to_string()], true, &BindOptions::default()).unwrap();
        assert_eq!(r, Value::Float(34.5));
    }

    #[test]
    fn structural() {
        let s = SchemaSet::new();
        let req = Shape::structural("urn:x", "echo", false);
        let x = encode(&s, &req, &Value::Array(vec![Value::from("hi"), Value::Int(2)]), &BindOptions::default()).unwrap();
        assert_eq!(x[0], "<ns1:echo xmlns:ns1=\"urn:x\"><arg0>hi</arg0><arg1>2</arg1></ns1:echo>");
        assert_eq!(decode(&s, &req, &x, false, &BindOptions::default()).unwrap(), Value::Array(vec![Value::from("hi"), Value::from("2")]));
        let resp = Shape::structural("urn:x", "echo", true);
        let r = encode(&s, &resp, &Value::from("ok"), &BindOptions::default()).unwrap();
        assert_eq!(decode(&s, &resp, &r, true, &BindOptions::default()).unwrap(), Value::from("ok"));
    }
}
