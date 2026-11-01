//! The `jq` language: an implementation of the jq 1.7 JSON query language.
//!
//! Supported: the full jq grammar — paths (`.`, `..`, `.foo`, `."foo"`, `.[e]`, `.[a:b]`,
//! `.[]`, `?`), pipes, `,`, `//`, arithmetic/comparison/boolean operators, assignment
//! operators (`=`, `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`), array/object construction,
//! string interpolation `"\(e)"`, formats (`@text @json @html @uri @csv @tsv @sh @base64
//! @base64d @base32 @base32d`, also as `@base64 "..\(e).."`), `if/elif/else/end` (optional
//! `else`), `try/catch`, `reduce`, `foreach` (2 and 3 clauses), `label $x | ... break $x`,
//! variables and destructuring (`. as [$a, {b: $c}] | ...`, alternatives `?//`), function
//! definitions with filter and `$value` parameters and recursion, `$__loc__`, `$ENV`, and
//! comments.
//!
//! Builtins: everything in jq 1.7's manual except module and I/O functions — e.g. `length
//! utf8bytelength not keys keys_unsorted has in inside map map_values path paths leaf_paths
//! getpath setpath delpaths del pick to_entries from_entries with_entries select empty
//! error add any all flatten range floor sqrt pow log ... tostring tonumber type infinite
//! nan isinfinite isnan isnormal sort sort_by group_by unique unique_by min max min_by
//! max_by reverse contains indices index rindex startswith endswith ltrimstr rtrimstr trim
//! ltrim rtrim explode implode split join ascii ascii_downcase ascii_upcase recurse env
//! transpose first last nth until while repeat limit isempty combinations walk tostream
//! fromstream truncate_stream tojson fromjson test match capture scan splits sub gsub
//! @formats todate fromdate date now mktime gmtime localtime strftime strflocaltime
//! strptime dateadd datesub todateiso8601 fromdateiso8601 abs toarray IN INDEX isvalid
//! builtins debug stderr input_line_number $__loc__`. Regexes use the crate's
//! Java-syntax engine with jq's flags `g i x n s p l`.
//!
//! Camel additions (as in camel-jq): `header(name)`, `header(name; default)`,
//! `property(name)`, `property(name; default)`, `variable(name)`.
//!
//! The result of an expression is its single output, `null` for no output, or an array of
//! all outputs. `input`/`inputs` raise "No more inputs" (there is one input per exchange).

mod builtins;
mod eval;
mod parse;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Expression, Language, Predicate};
use crate::value::Value;

use eval::{Bind, Ctl, Env, Interp, Pv};
use parse::{Ast, FuncDef};

pub(crate) use builtins::normalize;

fn prelude_defs() -> &'static [FuncDef] {
    static DEFS: OnceLock<Vec<FuncDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let toks = parse::lex(builtins::PRELUDE).expect("jq prelude lexes");
        let mut p = parse::Parser::new(builtins::PRELUDE, toks);
        match p.program().expect("jq prelude parses") {
            Ast::Defs(defs, _) => defs,
            _ => Vec::new(),
        }
    })
}

thread_local! {
    static PRELUDE_ENV: Env<'static> = {
        let mut env = Env::default();
        for d in prelude_defs() {
            env = env.push(Bind::Func(d));
        }
        env
    };
}

/// A prelude function by name/arity with the full prelude as its environment (so prelude
/// definitions may refer to each other regardless of order).
pub(crate) fn prelude_lookup(name: &str, arity: usize) -> Option<(&'static FuncDef, Env<'static>)> {
    let d = prelude_defs().iter().rev().find(|d| d.name == name && d.params.len() == arity)?;
    Some((d, PRELUDE_ENV.with(|e| e.clone())))
}

fn is_builtin(name: &str, arity: usize) -> bool {
    builtins::is_native(name, arity) || prelude_defs().iter().any(|d| d.name == name && d.params.len() == arity)
}

fn to_camel_error(c: Ctl) -> CamelError {
    match c {
        Ctl::Err(Value::String(s)) => CamelError::new("JqException", s),
        Ctl::Err(Value::Null) => CamelError::new("JqException", "null (null) not a string"),
        Ctl::Err(v) => CamelError::new("JqException", format!("{} (not a string)", eval::to_json(&v))),
        Ctl::Break(_) => CamelError::new("JqException", "break"),
        Ctl::Through(_, inner) => to_camel_error(*inner),
    }
}

/// A compiled jq program.
#[derive(Debug, Clone)]
pub struct Jq {
    text: String,
    expr: Ast,
}

impl Jq {
    /// Compile a jq program; syntax errors, undefined functions, variables and labels are
    /// reported here.
    pub fn compile(q: &str) -> Result<Jq> {
        let toks = parse::lex(q)?;
        let mut p = parse::Parser::new(q, toks);
        let expr = p.program()?;
        let native = |n: &str, a: usize| is_builtin(n, a);
        let mut scope = parse::Scope { funcs: Vec::new(), vars: Vec::new(), labels: Vec::new(), native: &native };
        parse::resolve(q, &expr, &mut scope)?;
        Ok(Jq { text: q.to_string(), expr })
    }

    /// The program text.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    fn run(&self, input: &Value, ex: Option<&Exchange>, f: &mut dyn FnMut(Value) -> std::result::Result<(), Ctl>) -> Result<()> {
        let v = normalize(super::jsonpath::to_json_value(input)?);
        let interp = Interp { ex };
        let env = PRELUDE_ENV.with(|e| e.clone());
        interp.eval(&self.expr, &env, Pv::val(v), &mut |x| f(x.v)).map_err(to_camel_error)
    }

    /// All outputs for an input.
    pub fn outputs(&self, input: &Value) -> Result<Vec<Value>> {
        self.outputs_with(input, None)
    }

    /// All outputs for an input, with an exchange for `header()`/`property()`.
    pub fn outputs_with(&self, input: &Value, ex: Option<&Exchange>) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        self.run(input, ex, &mut |v| {
            out.push(v);
            Ok(())
        })?;
        Ok(out)
    }

    /// Single output, null for none, array for several.
    pub fn evaluate(&self, input: &Value) -> Result<Value> {
        self.evaluate_with(input, None)
    }

    /// [`Jq::evaluate`] with an exchange for `header()`/`property()`.
    pub fn evaluate_with(&self, input: &Value, ex: Option<&Exchange>) -> Result<Value> {
        let mut outs = self.outputs_with(input, ex)?;
        Ok(match outs.len() {
            0 => Value::Null,
            1 => outs.pop().unwrap(),
            _ => Value::Array(outs),
        })
    }
}

/// Run a jq program against a value.
pub fn query(input: &Value, q: &str) -> Result<Value> {
    Jq::compile(q)?.evaluate(input)
}

struct JqExpression {
    jq: Jq,
    options: BTreeMap<String, String>,
}

impl Expression for JqExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let input = super::jsonpath::source_value(ex, &self.options);
        let v = self.jq.evaluate_with(&input, Some(ex))?;
        match self.options.get("resultType").filter(|t| !t.is_empty()) {
            Some(t) => super::support::convert_to_type_ex(Some(ex), &v, t),
            None => Ok(v),
        }
    }
}

impl Predicate for JqExpression {
    fn matches(&self, ex: &Exchange) -> Result<bool> {
        let input = super::jsonpath::source_value(ex, &self.options);
        Ok(self.jq.outputs_with(&input, Some(ex))?.iter().any(eval::truthy))
    }
}

/// The `jq` language.
#[derive(Default)]
pub struct JqLanguage;

impl Language for JqLanguage {
    fn name(&self) -> &str {
        "jq"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        Ok(Arc::new(JqExpression { jq: Jq::compile(text)?, options: options.clone() }))
    }
    fn create_predicate(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Predicate>> {
        Ok(Arc::new(JqExpression { jq: Jq::compile(text)?, options: options.clone() }))
    }
}

#[cfg(test)]
mod tests;
