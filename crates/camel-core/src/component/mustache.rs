//! `mustache:` component - renders a [Mustache](https://mustache.github.io/) template
//! (Camel `camel-mustache`).
//!
//! `mustache:templates/letter.mustache?allowTemplateFromHeader=false&contentCache=false`
//!
//! The rendered text becomes the message body. Template resources, the common options
//! and the data model (`body`, `headers`, `exchange.properties`, `variables`,
//! `request`...) are described in [`template_support`](super::template_support).
//! Headers `CamelMustacheTemplate` (template text) and `CamelMustacheResourceUri`
//! override the endpoint template when `allowTemplateFromHeader=true`.
//! `startDelimiter` / `endDelimiter` change the default `{{` `}}` delimiters.
//!
//! Supported Mustache features: escaped `{{name}}` and unescaped `{{{name}}}` /
//! `{{& name}}` variables, dotted names and the implicit iterator `{{.}}`, sections
//! over lists, objects and booleans, inverted sections, comments, partials
//! (`{{> name}}`, loaded from the template directory, `name` or `name.mustache`),
//! set-delimiter tags (`{{=<% %>=}}`) and standalone-line removal. Lambdas are not
//! supported. The engine is available standalone as [`render`].

use std::any::Any;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{CamelError, Result};
use crate::spi::{Component, Endpoint};
use crate::util::Parameters;
use crate::value::Value;

use super::template_support::{lookup_key, PartialLoader, TemplateEndpoint, TemplateEngine, TemplateHeaders, TemplateOptions};

/// Header carrying a template (requires `allowTemplateFromHeader=true`).
pub const MUSTACHE_TEMPLATE: &str = "CamelMustacheTemplate";
/// Header carrying a template resource uri (requires `allowTemplateFromHeader=true`).
pub const MUSTACHE_RESOURCE_URI: &str = "CamelMustacheResourceUri";

/// The `mustache` component.
#[derive(Default)]
pub struct MustacheComponent;

impl Component for MustacheComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let options = TemplateOptions::from_params(params)?;
        let engine = MustacheEngine {
            start: params.take("startDelimiter").unwrap_or_else(|| "{{".into()),
            end: params.take("endDelimiter").unwrap_or_else(|| "}}".into()),
        };
        let headers = TemplateHeaders { template: MUSTACHE_TEMPLATE, resource_uri: MUSTACHE_RESOURCE_URI, data_model: None, supplemental: None };
        Ok(Arc::new(TemplateEndpoint::new(ctx, uri, remaining, options, headers, Arc::new(engine))?))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The Mustache [`TemplateEngine`].
#[derive(Clone, Debug)]
pub struct MustacheEngine {
    pub start: String,
    pub end: String,
}

impl Default for MustacheEngine {
    fn default() -> Self {
        MustacheEngine { start: "{{".into(), end: "}}".into() }
    }
}

impl TemplateEngine for MustacheEngine {
    fn render(&self, template: &str, context: &Value, partials: PartialLoader<'_>) -> Result<String> {
        render_with(template, context, partials, &self.start, &self.end)
    }
}

/// Render a Mustache template against `context` (partials are not available).
pub fn render(template: &str, context: &Value) -> Result<String> {
    render_with(template, context, &|_| None, "{{", "}}")
}

/// Render with a partial loader and custom initial delimiters.
pub fn render_with(template: &str, context: &Value, partials: PartialLoader<'_>, start: &str, end: &str) -> Result<String> {
    let nodes = parse(template, start, end)?;
    let mut out = String::new();
    let mut stack = vec![context];
    render_nodes(&nodes, &mut stack, &mut out, partials, 0)?;
    Ok(out)
}

#[derive(Debug)]
enum Node {
    Text(String),
    Var { name: String, escape: bool },
    Section { name: String, inverted: bool, children: Vec<Node> },
    Partial { name: String, indent: String },
}

fn syntax(msg: impl std::fmt::Display) -> CamelError {
    CamelError::new(crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Mustache template error: {}", msg))
}

enum Flat {
    Text(String),
    Var { name: String, escape: bool },
    Open { name: String, inverted: bool },
    Close(String),
    Partial { name: String, indent: String },
}

fn parse(template: &str, start: &str, end: &str) -> Result<Vec<Node>> {
    let (mut otag, mut ctag) = (start.to_string(), end.to_string());
    let mut flat: Vec<Flat> = Vec::new();
    let mut pos = 0usize;
    while pos <= template.len() {
        let Some(rel) = template[pos..].find(&otag) else {
            flat.push(Flat::Text(template[pos..].to_string()));
            break;
        };
        let tag_start = pos + rel;
        let inner_start = tag_start + otag.len();
        let rest = &template[inner_start..];
        let (content, tag_end) = if rest.starts_with('{') {
            let close = format!("}}{}", ctag);
            let i = rest.find(&close).ok_or_else(|| syntax(format!("unclosed tag at offset {}", tag_start)))?;
            (format!("{{{}", &rest[1..i]), inner_start + i + close.len())
        } else {
            let i = rest.find(&ctag).ok_or_else(|| syntax(format!("unclosed tag at offset {}", tag_start)))?;
            (rest[..i].to_string(), inner_start + i + ctag.len())
        };
        let content = content.trim();
        let kind = content.chars().next().unwrap_or(' ');
        let standalone_kind = matches!(kind, '#' | '^' | '/' | '!' | '>' | '=');
        // standalone detection
        let line_start = template[..tag_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let mut text_end = tag_start;
        let mut next_pos = tag_end;
        let mut indent = String::new();
        if standalone_kind && line_start >= pos {
            let before = &template[line_start..tag_start];
            if before.chars().all(|c| c == ' ' || c == '\t') {
                let after = &template[tag_end..];
                let ws = after.len() - after.trim_start_matches([' ', '\t']).len();
                let after_ws = &after[ws..];
                let nl = if after_ws.starts_with("\r\n") {
                    Some(2)
                } else if after_ws.starts_with('\n') {
                    Some(1)
                } else if after_ws.is_empty() {
                    Some(0)
                } else {
                    None
                };
                if let Some(nl) = nl {
                    text_end = line_start;
                    next_pos = tag_end + ws + nl;
                    indent = before.to_string();
                }
            }
        }
        if text_end > pos {
            flat.push(Flat::Text(template[pos..text_end].to_string()));
        }
        let body = || content[1..].trim().to_string();
        match kind {
            '!' => {}
            '#' => flat.push(Flat::Open { name: body(), inverted: false }),
            '^' => flat.push(Flat::Open { name: body(), inverted: true }),
            '/' => flat.push(Flat::Close(body())),
            '>' => flat.push(Flat::Partial { name: body(), indent }),
            '&' => flat.push(Flat::Var { name: body(), escape: false }),
            '{' => flat.push(Flat::Var { name: content[1..].trim().to_string(), escape: false }),
            '=' => {
                let d = content.trim_start_matches('=').trim_end_matches('=').trim();
                let parts: Vec<&str> = d.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err(syntax(format!("invalid set delimiter tag '{}'", content)));
                }
                otag = parts[0].to_string();
                ctag = parts[1].to_string();
            }
            _ => {
                if content.is_empty() {
                    return Err(syntax(format!("empty tag at offset {}", tag_start)));
                }
                flat.push(Flat::Var { name: content.to_string(), escape: true })
            }
        }
        pos = next_pos;
        if pos == template.len() {
            break;
        }
    }
    // build the tree
    let mut stack: Vec<(String, bool, Vec<Node>)> = vec![(String::new(), false, Vec::new())];
    for f in flat {
        match f {
            Flat::Text(t) => stack.last_mut().unwrap().2.push(Node::Text(t)),
            Flat::Var { name, escape } => stack.last_mut().unwrap().2.push(Node::Var { name, escape }),
            Flat::Partial { name, indent } => stack.last_mut().unwrap().2.push(Node::Partial { name, indent }),
            Flat::Open { name, inverted } => stack.push((name, inverted, Vec::new())),
            Flat::Close(name) => {
                if stack.len() == 1 {
                    return Err(syntax(format!("unexpected closing tag '{}'", name)));
                }
                let (open, inverted, children) = stack.pop().unwrap();
                if open != name {
                    return Err(syntax(format!("section '{}' closed by '{}'", open, name)));
                }
                stack.last_mut().unwrap().2.push(Node::Section { name, inverted, children });
            }
        }
    }
    if stack.len() > 1 {
        return Err(syntax(format!("unclosed section '{}'", stack.last().unwrap().0)));
    }
    Ok(stack.pop().unwrap().2)
}

fn lookup<'a>(name: &str, stack: &[&'a Value]) -> Option<&'a Value> {
    if name == "." {
        return stack.last().copied();
    }
    let mut segs = name.split('.');
    let first = segs.next()?;
    let mut cur = stack.iter().rev().find_map(|ctx| match ctx {
        Value::Object(m) => m.get(first).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(first)).map(|(_, v)| v)),
        _ => None,
    })?;
    for s in segs {
        cur = lookup_key(cur, s)?;
    }
    Some(cur)
}

fn is_falsey(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => true,
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    }
}

/// HTML-escape like Mustache: `& < > " '`.
pub fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

fn render_nodes(nodes: &[Node], stack: &mut Vec<&Value>, out: &mut String, partials: PartialLoader<'_>, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(syntax("partials nested too deeply"));
    }
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Var { name, escape } => {
                if let Some(v) = lookup(name, stack) {
                    let s = v.to_string();
                    if *escape {
                        escape_html(&s, out);
                    } else {
                        out.push_str(&s);
                    }
                }
            }
            Node::Section { name, inverted, children } => {
                let v = lookup(name, stack);
                if *inverted {
                    if is_falsey(v) {
                        render_nodes(children, stack, out, partials, depth)?;
                    }
                    continue;
                }
                if is_falsey(v) {
                    continue;
                }
                match v.unwrap() {
                    Value::Array(items) => {
                        for item in items {
                            stack.push(item);
                            let r = render_nodes(children, stack, out, partials, depth);
                            stack.pop();
                            r?;
                        }
                    }
                    Value::Bool(true) => render_nodes(children, stack, out, partials, depth)?,
                    other => {
                        stack.push(other);
                        let r = render_nodes(children, stack, out, partials, depth);
                        stack.pop();
                        r?;
                    }
                }
            }
            Node::Partial { name, indent } => {
                let Some(text) = partials(name) else { continue };
                let text = if indent.is_empty() {
                    text
                } else {
                    let mut s = String::new();
                    for line in text.split_inclusive('\n') {
                        s.push_str(indent);
                        s.push_str(line);
                    }
                    s
                };
                let nodes = parse(&text, "{{", "}}")?;
                render_nodes(&nodes, stack, out, partials, depth + 1)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn r(t: &str, data: &str) -> String {
        render(t, &json::parse(data).unwrap()).unwrap()
    }

    #[test]
    fn variables() {
        assert_eq!(r("Hello {{name}}!", r#"{"name":"World"}"#), "Hello World!");
        assert_eq!(r("{{a}}|{{{a}}}|{{& a}}", r#"{"a":"<b>&\"'"}"#), "&lt;b&gt;&amp;&quot;&#39;|<b>&\"'|<b>&\"'");
        assert_eq!(r("{{missing}}x", "{}"), "x");
        assert_eq!(r("{{n}} {{f}} {{b}}", r#"{"n":42,"f":1.5,"b":true}"#), "42 1.5 true");
        assert_eq!(r("{{a.b.c}}", r#"{"a":{"b":{"c":"deep"}}}"#), "deep");
        assert_eq!(r("{{ spaced }}", r#"{"spaced":"ok"}"#), "ok");
        assert_eq!(r("{{list.1}}", r#"{"list":["x","y"]}"#), "y");
    }

    #[test]
    fn sections() {
        assert_eq!(r("{{#items}}<{{name}}>{{/items}}", r#"{"items":[{"name":"a"},{"name":"b"}]}"#), "<a><b>");
        assert_eq!(r("{{#list}}{{.}},{{/list}}", r#"{"list":[1,2,3]}"#), "1,2,3,");
        assert_eq!(r("{{#flag}}yes{{/flag}}{{^flag}}no{{/flag}}", r#"{"flag":true}"#), "yes");
        assert_eq!(r("{{#flag}}yes{{/flag}}{{^flag}}no{{/flag}}", r#"{"flag":false}"#), "no");
        assert_eq!(r("{{^list}}empty{{/list}}", r#"{"list":[]}"#), "empty");
        assert_eq!(r("{{^missing}}none{{/missing}}", "{}"), "none");
        assert_eq!(r("{{#person}}{{name}} is {{age}}{{/person}}", r#"{"person":{"name":"Al","age":3}}"#), "Al is 3");
        // context stack: outer names visible inside sections
        assert_eq!(r("{{#items}}{{name}}-{{sep}};{{/items}}", r#"{"sep":"!","items":[{"name":"a"}]}"#), "a-!;");
        // nested sections
        assert_eq!(
            r("{{#a}}{{#b}}{{c}}{{/b}}{{/a}}", r#"{"a":[{"b":[{"c":1},{"c":2}]},{"b":[{"c":3}]}]}"#),
            "123"
        );
        assert_eq!(r("{{#a.b}}{{.}}{{/a.b}}", r#"{"a":{"b":"x"}}"#), "x");
    }

    #[test]
    fn comments_standalone_and_delimiters() {
        assert_eq!(r("a{{! comment }}b", "{}"), "ab");
        assert_eq!(r("Begin.\n{{! standalone comment }}\nEnd.\n", "{}"), "Begin.\nEnd.\n");
        assert_eq!(r("| This Is\n{{#b}}\n|\n{{/b}}\n| A Line\n", r#"{"b":true}"#), "| This Is\n|\n| A Line\n");
        assert_eq!(r("  {{#b}}\n  x\n  {{/b}}\n", r#"{"b":true}"#), "  x\n");
        assert_eq!(r(" | {{#b}} {{/b}} |\n", r#"{"b":true}"#), " |   |\n");
        assert_eq!(r("{{=<% %>=}}<% name %> {{name}}", r#"{"name":"x"}"#), "x {{name}}");
        assert_eq!(r("{{=| |=}}\n|a|\n|={{ }}=|\n{{a}}", r#"{"a":"1"}"#), "1\n1");
        let e = MustacheEngine { start: "<%".into(), end: "%>".into() };
        assert_eq!(e.render("<%x%>", &json::parse(r#"{"x":5}"#).unwrap(), &|_| None).unwrap(), "5");
    }

    #[test]
    fn partials() {
        let data = json::parse(r#"{"people":[{"name":"a"},{"name":"b"}]}"#).unwrap();
        let p = |n: &str| if n == "person" { Some("<{{name}}>".to_string()) } else { None };
        assert_eq!(render_with("{{#people}}{{> person}}{{/people}}", &data, &p, "{{", "}}").unwrap(), "<a><b>");
        let p = |_: &str| Some("x\ny\n".to_string());
        assert_eq!(render_with("  {{> p}}\n", &data, &p, "{{", "}}").unwrap(), "  x\n  y\n");
    }

    #[test]
    fn errors() {
        assert!(render("{{#a}}x", &Value::Null).is_err());
        assert!(render("{{#a}}x{{/b}}", &Value::Null).is_err());
        assert!(render("{{/a}}", &Value::Null).is_err());
        assert!(render("{{a", &Value::Null).is_err());
    }
}
