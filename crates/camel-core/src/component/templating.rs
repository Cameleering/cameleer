//! Minimal template engines: `string-template:`, `velocity:` and `freemarker:`.
//!
//! These are **substitution-only** implementations of the Camel components of the same
//! names (rust-camel has no StringTemplate/Velocity/FreeMarker engine). They share the
//! data model and the options of [`template_support`](super::template_support)
//! (`body`, `headers.x`, `exchange.properties.x`, `variables.x`, `request.body`, ...).
//!
//! - `string-template:file.st?delimiterStart=<&delimiterStop=>` — `<name>` and
//!   `<a.b>` attribute references, `<! comment !>`, `\<` escapes. Template
//!   expressions (`<if(...)>`, `<names:{...}>`, ...) fail with "not supported".
//!   Headers `CamelStringTemplateTemplate`/`CamelStringTemplateResourceUri`.
//! - `velocity:file.vm` — `$name`, `${name}`, `$a.b`, quiet references `$!name` /
//!   `$!{name}`; unknown references render literally (as in Velocity); `##` line
//!   comments and `#* *#` block comments. Directives (`#if`, `#foreach`, `#set`, ...)
//!   fail with "not supported". Headers `CamelVelocityTemplate`/`CamelVelocityResourceUri`.
//! - `freemarker:file.ftl` — `${name}`, `${a.b}`, defaults `${name!"x"}` / `${name!}`;
//!   a missing value without default is an error (as in FreeMarker); `<#-- -->`
//!   comments. Directives (`<#if>`, `<#list>`, ...) fail with "not supported".
//!   Headers `CamelFreemarkerTemplate`/`CamelFreemarkerResourceUri`.

use std::any::Any;
use std::sync::Arc;

use crate::context::CamelContext;
use crate::error::{kinds, CamelError, Result};
use crate::spi::{Component, Endpoint};
use crate::util::Parameters;
use crate::value::Value;

use super::template_support::{lookup_path, PartialLoader, TemplateEndpoint, TemplateEngine, TemplateHeaders, TemplateOptions};

fn unsupported(engine: &str, what: &str) -> CamelError {
    CamelError::new(
        kinds::UNSUPPORTED_OPERATION,
        format!("{} is not supported by the rust-camel {} component (only variable substitution is implemented)", what, engine),
    )
}

fn create(
    ctx: &CamelContext,
    uri: &str,
    remaining: &str,
    params: &mut Parameters,
    headers: TemplateHeaders,
    engine: Arc<dyn TemplateEngine>,
) -> Result<Arc<dyn Endpoint>> {
    let options = TemplateOptions::from_params(params)?;
    Ok(Arc::new(TemplateEndpoint::new(ctx, uri, remaining, options, headers, engine)?))
}

// ---------------------------------------------------------------------------
// StringTemplate
// ---------------------------------------------------------------------------

/// The `string-template` component.
#[derive(Default)]
pub struct StringTemplateComponent;

impl Component for StringTemplateComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let engine = StringTemplateEngine {
            start: params.take("delimiterStart").and_then(|s| s.chars().next()).unwrap_or('<'),
            stop: params.take("delimiterStop").and_then(|s| s.chars().next()).unwrap_or('>'),
        };
        let headers = TemplateHeaders { template: "CamelStringTemplateTemplate", resource_uri: "CamelStringTemplateResourceUri" };
        create(ctx, uri, remaining, params, headers, Arc::new(engine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Minimal StringTemplate engine (attribute substitution only).
#[derive(Clone, Debug)]
pub struct StringTemplateEngine {
    pub start: char,
    pub stop: char,
}

impl Default for StringTemplateEngine {
    fn default() -> Self {
        StringTemplateEngine { start: '<', stop: '>' }
    }
}

impl TemplateEngine for StringTemplateEngine {
    fn render(&self, template: &str, context: &Value, _partials: PartialLoader<'_>) -> Result<String> {
        let mut out = String::new();
        let chars: Vec<char> = template.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && i + 1 < chars.len() && (chars[i + 1] == self.start || chars[i + 1] == '\\') {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c != self.start {
                out.push(c);
                i += 1;
                continue;
            }
            // comment <! ... !>
            if chars.get(i + 1) == Some(&'!') {
                let mut j = i + 2;
                while j + 1 < chars.len() && !(chars[j] == '!' && chars[j + 1] == self.stop) {
                    j += 1;
                }
                i = j + 2;
                continue;
            }
            let Some(rel) = chars[i + 1..].iter().position(|&x| x == self.stop) else {
                return Err(CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, "StringTemplate: unclosed expression"));
            };
            let expr: String = chars[i + 1..i + 1 + rel].iter().collect();
            let expr = expr.trim();
            if expr.is_empty() || !expr.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                return Err(unsupported("string-template", &format!("The template expression <{}>", expr)));
            }
            if let Some(v) = lookup_path(context, expr) {
                render_st_value(v, &mut out);
            }
            i += rel + 2;
        }
        Ok(out)
    }
}

fn render_st_value(v: &Value, out: &mut String) {
    match v {
        // StringTemplate renders multi-valued attributes concatenated
        Value::Array(a) => a.iter().for_each(|x| render_st_value(x, out)),
        other => out.push_str(&other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Velocity
// ---------------------------------------------------------------------------

/// The `velocity` component.
#[derive(Default)]
pub struct VelocityComponent;

impl Component for VelocityComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        for k in ["loaderCache", "propertiesFile"] {
            params.take(k);
        }
        let headers = TemplateHeaders { template: "CamelVelocityTemplate", resource_uri: "CamelVelocityResourceUri" };
        create(ctx, uri, remaining, params, headers, Arc::new(VelocityEngine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Minimal Velocity engine (references only).
#[derive(Clone, Copy, Debug, Default)]
pub struct VelocityEngine;

const VELOCITY_DIRECTIVES: &[&str] =
    &["if", "elseif", "else", "end", "foreach", "set", "macro", "parse", "include", "stop", "break", "evaluate", "define"];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

impl TemplateEngine for VelocityEngine {
    fn render(&self, template: &str, context: &Value, _partials: PartialLoader<'_>) -> Result<String> {
        let chars: Vec<char> = template.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && chars.get(i + 1) == Some(&'$') {
                out.push('$');
                i += 2;
                continue;
            }
            if c == '#' {
                match chars.get(i + 1) {
                    Some('#') => {
                        // line comment: drop the rest of the line including the newline
                        while i < chars.len() && chars[i] != '\n' {
                            i += 1;
                        }
                        i += 1;
                        continue;
                    }
                    Some('*') => {
                        let mut j = i + 2;
                        while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '#') {
                            j += 1;
                        }
                        i = j + 2;
                        continue;
                    }
                    _ => {
                        let mut j = i + 1;
                        if chars.get(j) == Some(&'{') {
                            j += 1;
                        }
                        let word: String = chars[j..].iter().take_while(|c| c.is_ascii_alphabetic()).collect();
                        if VELOCITY_DIRECTIVES.contains(&word.as_str()) {
                            return Err(unsupported("velocity", &format!("The directive #{}", word)));
                        }
                    }
                }
            }
            if c != '$' {
                out.push(c);
                i += 1;
                continue;
            }
            // reference
            let start = i;
            let mut j = i + 1;
            let quiet = chars.get(j) == Some(&'!');
            if quiet {
                j += 1;
            }
            let braced = chars.get(j) == Some(&'{');
            if braced {
                j += 1;
            }
            if !chars.get(j).map(|&c| is_ident_start(c)).unwrap_or(false) {
                out.push(c);
                i += 1;
                continue;
            }
            let mut name = String::new();
            loop {
                while j < chars.len() && is_ident(chars[j]) {
                    name.push(chars[j]);
                    j += 1;
                }
                // dotted property access: only when followed by an identifier
                if j + 1 < chars.len() && chars[j] == '.' && is_ident_start(chars[j + 1]) {
                    name.push('.');
                    j += 1;
                    continue;
                }
                break;
            }
            if braced {
                if chars.get(j) != Some(&'}') {
                    return Err(CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Velocity: unclosed reference at offset {}", start)));
                }
                j += 1;
            }
            if chars.get(j) == Some(&'(') {
                return Err(unsupported("velocity", &format!("The method call ${}(...)", name)));
            }
            match lookup_path(context, &name) {
                Some(v) if !v.is_null() => out.push_str(&v.to_string()),
                _ if quiet => {}
                _ => out.extend(chars[start..j].iter()),
            }
            i = j;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// FreeMarker
// ---------------------------------------------------------------------------

/// The `freemarker` component.
#[derive(Default)]
pub struct FreemarkerComponent;

impl Component for FreemarkerComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        for k in ["configuration", "templateUpdateDelay", "localizedLookup"] {
            params.take(k);
        }
        let headers = TemplateHeaders { template: "CamelFreemarkerTemplate", resource_uri: "CamelFreemarkerResourceUri" };
        create(ctx, uri, remaining, params, headers, Arc::new(FreemarkerEngine))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Minimal FreeMarker engine (interpolations only).
#[derive(Clone, Copy, Debug, Default)]
pub struct FreemarkerEngine;

impl TemplateEngine for FreemarkerEngine {
    fn render(&self, template: &str, context: &Value, _partials: PartialLoader<'_>) -> Result<String> {
        let mut out = String::new();
        let mut rest = template;
        loop {
            let next_interp = rest.find("${");
            let next_tag = ["<#", "</#", "[#", "[/#"].iter().filter_map(|t| rest.find(t)).min();
            match (next_interp, next_tag) {
                (None, None) => {
                    out.push_str(rest);
                    break;
                }
                (i, Some(t)) if i.map(|i| t < i).unwrap_or(true) => {
                    out.push_str(&rest[..t]);
                    let r = &rest[t..];
                    if r.starts_with("<#--") {
                        let end = r.find("-->").ok_or_else(|| CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, "FreeMarker: unclosed comment"))?;
                        rest = &r[end + 3..];
                        continue;
                    }
                    let word: String = r.trim_start_matches(['<', '[', '/', '#']).chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                    return Err(unsupported("freemarker", &format!("The directive #{}", word)));
                }
                (Some(i), _) => {
                    out.push_str(&rest[..i]);
                    let r = &rest[i + 2..];
                    let end = r.find('}').ok_or_else(|| CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, "FreeMarker: unclosed ${"))?;
                    let expr = r[..end].trim();
                    let (path, default) = match expr.split_once('!') {
                        Some((p, d)) => (p.trim(), Some(d.trim().trim_matches('"').trim_matches('\''))),
                        None => (expr, None),
                    };
                    if !path.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                        return Err(unsupported("freemarker", &format!("The expression ${{{}}}", expr)));
                    }
                    match (lookup_path(context, path), default) {
                        (Some(v), _) if !v.is_null() => out.push_str(&v.to_string()),
                        (_, Some(d)) => out.push_str(d),
                        _ => {
                            return Err(CamelError::new(
                                kinds::EXPRESSION_EVALUATION,
                                format!("FreeMarker: the following has evaluated to null or missing: ${{{}}}", expr),
                            ))
                        }
                    }
                    rest = &r[end + 1..];
                }
                (None, Some(_)) => unreachable!(),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn data() -> Value {
        json::parse(r#"{"body":"B","headers":{"name":"N","list":["a","b"]},"exchange":{"properties":{"p":1}}}"#).unwrap()
    }

    #[test]
    fn string_template() {
        let e = StringTemplateEngine::default();
        let d = data();
        assert_eq!(e.render("Hi <headers.name>, body=<body> <! c !>p=<exchange.properties.p> \\<x", &d, &|_| None).unwrap(), "Hi N, body=B p=1 <x");
        assert_eq!(e.render("<headers.list>|<missing>|", &d, &|_| None).unwrap(), "ab||");
        assert!(e.render("<if(x)>y<endif>", &d, &|_| None).is_err());
        let e = StringTemplateEngine { start: '$', stop: '$' };
        assert_eq!(e.render("$body$!", &d, &|_| None).unwrap(), "B!");
    }

    #[test]
    fn velocity() {
        let e = VelocityEngine;
        let d = data();
        assert_eq!(e.render("Dear $headers.name. ${body}! $!missing|$missing|\\$body ## comment\nx", &d, &|_| None).unwrap(), "Dear N. B! |$missing|$body x");
        assert_eq!(e.render("cost: $5 #* block *#ok", &d, &|_| None).unwrap(), "cost: $5 ok");
        assert!(e.render("#if($body)x#end", &d, &|_| None).is_err());
        assert!(e.render("$body.toUpperCase()", &d, &|_| None).is_err());
    }

    #[test]
    fn freemarker() {
        let e = FreemarkerEngine;
        let d = data();
        assert_eq!(e.render("Hi ${headers.name} ${body} ${missing!\"dflt\"}${missing!} <#-- c -->.", &d, &|_| None).unwrap(), "Hi N B dflt .");
        assert!(e.render("${missing}", &d, &|_| None).is_err());
        assert!(e.render("<#if x>y</#if>", &d, &|_| None).is_err());
    }
}
