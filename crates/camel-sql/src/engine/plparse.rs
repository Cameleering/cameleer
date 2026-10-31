//! Parsing of `CREATE PROCEDURE` / `CREATE FUNCTION`, `CALL`, `DO` and the procedural
//! body language (a PL/pgSQL subset).

use super::ast::*;
use super::lexer::{Tok, Token};
use super::parser::{parse_statements, Parser};
use super::{SqlErr, SqlResult};

const MODES: &[&str] = &["IN", "OUT", "INOUT", "VARIADIC"];
const RAISE_LEVELS: &[&str] = &["EXCEPTION", "NOTICE", "WARNING", "INFO", "LOG", "DEBUG"];

/// Parse a routine body (procedural block or SQL statements).
pub fn parse_body(text: &str) -> SqlResult<RoutineBody> {
    let t = text.trim_start();
    let up: String = t.chars().take(8).collect::<String>().to_ascii_uppercase();
    if up.starts_with("DECLARE") || up.starts_with("BEGIN") || up.starts_with("<<") {
        let mut p = Parser::new(text)?;
        let label = p.opt_label()?;
        let b = p.pl_block(label)?;
        while p.eat_op(";") {}
        if !p.at_eof() {
            return Err(p.unexpected("end of routine body"));
        }
        return Ok(RoutineBody::Pl(b));
    }
    Ok(RoutineBody::Sql(parse_statements(text)?))
}

impl Parser {
    /// A parser over a token list (an end-of-input token is appended if missing).
    pub(crate) fn from_tokens(mut toks: Vec<Token>) -> Parser {
        if !matches!(toks.last().map(|t| &t.tok), Some(Tok::Eof)) {
            let (line, col) = toks.last().map(|t| (t.line, t.col)).unwrap_or((1, 1));
            toks.push(Token { tok: Tok::Eof, line, col });
        }
        Parser { toks, pos: 0, next_param: 0 }
    }

    fn word(&self) -> Option<String> {
        match &self.peek().tok {
            Tok::Ident(s) => Some(s.to_ascii_uppercase()),
            _ => None,
        }
    }

    /// Any identifier, reserved words included (labels), lower-cased.
    fn any_ident(&mut self, what: &str) -> SqlResult<String> {
        match &self.peek().tok {
            Tok::Ident(s) | Tok::QIdent(s) => {
                let s = s.to_ascii_lowercase();
                self.advance();
                Ok(s)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    // ------------------------------------------------------------------ statements

    pub(crate) fn call_stmt(&mut self) -> SqlResult<Statement> {
        self.advance(); // CALL / EXEC
        let name = self.table_name()?.to_ascii_lowercase();
        let mut args = Vec::new();
        if self.eat_op("(") {
            if !self.eat_op(")") {
                loop {
                    // named notation `name => value` is accepted positionally
                    if matches!(self.peek().tok, Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Op("=>")) {
                        self.advance();
                        self.advance();
                    }
                    args.push(self.expr()?);
                    if self.eat_op(")") {
                        break;
                    }
                    self.expect_op(",")?;
                }
            }
        } else if !self.at_eof() && !self.is_op(";") {
            // EXEC name arg1, arg2
            loop {
                args.push(self.expr()?);
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        Ok(Statement::Call { name, args })
    }

    pub(crate) fn do_stmt(&mut self) -> SqlResult<Statement> {
        self.advance(); // DO
        if self.eat_kw("LANGUAGE") {
            self.advance();
        }
        let body = match self.advance().tok {
            Tok::Str(s) => s,
            _ => return Err(SqlErr::syntax("DO requires a dollar-quoted or string body")),
        };
        if self.eat_kw("LANGUAGE") {
            self.advance();
        }
        match parse_body(&body)? {
            RoutineBody::Pl(b) => Ok(Statement::Do(Box::new(b))),
            RoutineBody::Sql(_) => Err(SqlErr::syntax("DO requires a procedural block (BEGIN ... END)")),
        }
    }

    fn param_type(&mut self) -> SqlResult<DataType> {
        let (ty, _) = self.data_type()?;
        // `%TYPE` references are accepted (the declared type is used as-is)
        if self.eat_op("%") {
            self.advance();
        }
        Ok(ty)
    }

    pub(crate) fn create_routine(&mut self, _or_replace: bool) -> SqlResult<Statement> {
        let function = self.advance().describe().eq_ignore_ascii_case("FUNCTION");
        let name = self.table_name()?.to_ascii_lowercase();
        self.expect_op("(")?;
        let mut params: Vec<RoutineParam> = Vec::new();
        if !self.eat_op(")") {
            loop {
                let mut mode = ParamMode::In;
                if let Some(w) = self.word().filter(|w| MODES.contains(&w.as_str())) {
                    if w == "VARIADIC" {
                        return Err(SqlErr::new("0A000", "VARIADIC parameters are not supported"));
                    }
                    self.advance();
                    mode = match w.as_str() {
                        "OUT" => ParamMode::Out,
                        "INOUT" => ParamMode::InOut,
                        _ => ParamMode::In,
                    };
                }
                // [name] [mode] type
                let mut pname = None;
                let unnamed_type = matches!((self.word().as_deref(), self.peek_at(1)), (Some("DOUBLE"), Tok::Ident(s)) if s.eq_ignore_ascii_case("PRECISION"))
                    || matches!((self.word().as_deref(), self.peek_at(1)), (Some("CHARACTER"), Tok::Ident(s)) if s.eq_ignore_ascii_case("VARYING"));
                if !unnamed_type && matches!(self.peek().tok, Tok::Ident(_) | Tok::QIdent(_)) && matches!(self.peek_at(1), Tok::Ident(_)) {
                    pname = Some(self.ident("parameter name")?.to_ascii_lowercase());
                    if let Some(w) = self.word().filter(|w| MODES.contains(&w.as_str()) && w != "VARIADIC") {
                        if matches!(self.peek_at(1), Tok::Ident(_)) {
                            self.advance();
                            mode = match w.as_str() {
                                "OUT" => ParamMode::Out,
                                "INOUT" => ParamMode::InOut,
                                _ => ParamMode::In,
                            };
                        }
                    }
                }
                let ty = self.param_type()?;
                let default = if self.eat_kw("DEFAULT") || self.eat_op("=") { Some(self.expr()?) } else { None };
                params.push(RoutineParam { name: pname, mode, ty, default });
                if self.eat_op(")") {
                    break;
                }
                self.expect_op(",")?;
            }
        }
        let mut returns: Option<Returns> = None;
        let mut body: Option<String> = None;
        let mut strict = false;
        let mut language = String::from("sql");
        loop {
            if self.at_eof() || self.is_op(";") {
                break;
            }
            let Some(w) = self.word() else {
                return Err(self.unexpected("a routine attribute (RETURNS, LANGUAGE, AS ...)"));
            };
            match w.as_str() {
                "RETURNS" => {
                    self.advance();
                    if self.eat_kw("NULL") {
                        self.expect_kw("ON")?;
                        self.expect_kw("NULL")?;
                        self.expect_kw("INPUT")?;
                        strict = true;
                        continue;
                    }
                    if self.eat_kw("TABLE") {
                        self.expect_op("(")?;
                        let mut cols = Vec::new();
                        loop {
                            let c = self.ident("column name")?.to_ascii_lowercase();
                            let ty = self.param_type()?;
                            cols.push((c, ty));
                            if self.eat_op(")") {
                                break;
                            }
                            self.expect_op(",")?;
                        }
                        returns = Some(Returns::Table(cols));
                    } else if self.eat_kw("SETOF") {
                        let save = self.pos;
                        match self.data_type() {
                            Ok((ty, _)) => returns = Some(Returns::SetOf(Some(ty), None)),
                            Err(_) => {
                                self.pos = save;
                                let t = self.table_name()?.to_ascii_lowercase();
                                returns = Some(if t == "record" { Returns::SetOf(None, None) } else { Returns::SetOf(None, Some(t)) });
                            }
                        }
                    } else if self.eat_kw("VOID") {
                        returns = Some(Returns::Void);
                    } else if self.eat_kw("RECORD") {
                        returns = Some(Returns::Record);
                    } else if self.is_kw("TRIGGER") {
                        return Err(SqlErr::new("0A000", "trigger functions are not supported"));
                    } else {
                        returns = Some(Returns::Scalar(self.param_type()?));
                    }
                }
                "LANGUAGE" => {
                    self.advance();
                    language = self.advance().describe().trim_matches('\'').to_ascii_lowercase();
                }
                "AS" => {
                    self.advance();
                    match self.advance().tok {
                        Tok::Str(s) => body = Some(s),
                        _ => return Err(SqlErr::syntax("routine body must be a string or dollar-quoted constant")),
                    }
                    if self.eat_op(",") {
                        self.advance(); // C function link symbol: ignored
                    }
                }
                "STRICT" => {
                    self.advance();
                    strict = true;
                }
                "CALLED" => {
                    self.advance();
                    self.expect_kw("ON")?;
                    self.expect_kw("NULL")?;
                    self.expect_kw("INPUT")?;
                }
                "IMMUTABLE" | "STABLE" | "VOLATILE" | "LEAKPROOF" | "EXTERNAL" | "DETERMINISTIC" => {
                    self.advance();
                }
                "NOT" => {
                    self.advance();
                    self.eat_kw("LEAKPROOF");
                    self.eat_kw("DETERMINISTIC");
                }
                "SECURITY" | "PARALLEL" | "COST" | "ROWS" => {
                    self.advance();
                    self.advance();
                }
                "SET" => {
                    // SET configuration_parameter { TO | = } value | FROM CURRENT
                    self.advance();
                    while !self.at_eof() && !self.is_op(";") && !self.is_kw("AS") && !self.is_kw("LANGUAGE") {
                        self.advance();
                    }
                }
                _ => return Err(self.unexpected("a routine attribute (RETURNS, LANGUAGE, AS ...)")),
            }
        }
        if !matches!(language.as_str(), "sql" | "plpgsql" | "plsql" | "sqlscript") {
            return Err(SqlErr::new("42704", format!("language \"{}\" does not exist (use sql or plpgsql)", language)));
        }
        let body_text = body.ok_or_else(|| SqlErr::syntax(format!("no routine body specified for \"{}\"", name)))?;
        let has_out = params.iter().any(|p| p.mode != ParamMode::In);
        let returns = match (function, returns) {
            (false, Some(_)) => return Err(SqlErr::syntax("procedures cannot have a RETURNS clause (use OUT parameters)")),
            (false, None) => Returns::Void,
            (true, Some(r)) => r,
            (true, None) if has_out => Returns::Record,
            (true, None) => return Err(SqlErr::new("42P13", "function result type must be specified")),
        };
        let body = parse_body(&body_text)?;
        Ok(Statement::CreateRoutine { routine: Box::new(Routine { name, function, params, returns, body, strict }), or_replace: _or_replace })
    }

    // ------------------------------------------------------------------ procedural language

    /// `<<label>>`
    pub(crate) fn opt_label(&mut self) -> SqlResult<Option<String>> {
        if self.is_op("<") && matches!(self.peek_at(1), Tok::Op("<")) {
            self.advance();
            self.advance();
            let l = self.any_ident("label")?;
            self.expect_op(">")?;
            self.expect_op(">")?;
            return Ok(Some(l));
        }
        Ok(None)
    }

    pub(crate) fn pl_block(&mut self, label: Option<String>) -> SqlResult<Block> {
        let mut decls = Vec::new();
        while self.eat_kw("DECLARE") {
            while !self.is_kw("BEGIN") && !self.is_kw("DECLARE") && !self.at_eof() {
                decls.push(self.pl_decl()?);
            }
        }
        self.expect_kw("BEGIN")?;
        let body = self.pl_stmts(&["END", "EXCEPTION"])?;
        let mut handlers = Vec::new();
        if self.eat_kw("EXCEPTION") {
            while self.eat_kw("WHEN") {
                let mut conds = Vec::new();
                loop {
                    if self.eat_kw("SQLSTATE") {
                        match self.advance().tok {
                            Tok::Str(s) => conds.push(format!("sqlstate:{}", s.to_ascii_uppercase())),
                            _ => return Err(SqlErr::syntax("SQLSTATE requires a string code")),
                        }
                    } else {
                        conds.push(self.ident("condition name")?.to_ascii_lowercase());
                    }
                    if !self.eat_kw("OR") {
                        break;
                    }
                }
                self.expect_kw("THEN")?;
                let stmts = self.pl_stmts(&["WHEN", "END"])?;
                handlers.push((conds, stmts));
            }
        }
        self.expect_kw("END")?;
        if let Some(l) = &label {
            if matches!(&self.peek().tok, Tok::Ident(s) if s.eq_ignore_ascii_case(l)) {
                self.advance();
            }
        }
        Ok(Block { label, decls, body, handlers })
    }

    fn pl_decl(&mut self) -> SqlResult<VarDecl> {
        let name = self.ident("variable name")?.to_ascii_lowercase();
        let constant = self.eat_kw("CONSTANT");
        let ty = if self.eat_kw("RECORD") {
            None
        } else if self.eat_kw("ALIAS") {
            self.expect_kw("FOR")?;
            self.advance();
            self.expect_op(";")?;
            return Err(SqlErr::new("0A000", "ALIAS FOR declarations are not supported (use the parameter names or $n)"));
        } else if matches!(self.peek().tok, Tok::Ident(_) | Tok::QIdent(_)) && (matches!(self.peek_at(1), Tok::Op("%")) || (matches!(self.peek_at(1), Tok::Op(".")) && matches!(self.peek_at(3), Tok::Op("%")))) {
            // t%ROWTYPE / t.col%TYPE: untyped
            while !self.is_op("%") {
                self.advance();
            }
            self.advance();
            self.advance();
            None
        } else {
            Some(self.param_type()?)
        };
        let mut not_null = false;
        if self.eat_kw("NOT") {
            self.expect_kw("NULL")?;
            not_null = true;
        }
        let default = if self.eat_op(":=") || self.eat_op("=") || self.eat_kw("DEFAULT") { Some(self.expr()?) } else { None };
        self.expect_op(";")?;
        Ok(VarDecl { name, ty, default, constant, not_null })
    }

    fn pl_stmts(&mut self, terminators: &[&str]) -> SqlResult<Vec<PlStmt>> {
        let mut out = Vec::new();
        loop {
            if self.at_eof() {
                return Err(self.unexpected(&terminators.join(" or ")));
            }
            if terminators.iter().any(|t| self.is_kw(t)) {
                return Ok(out);
            }
            out.push(self.pl_stmt()?);
        }
    }

    /// Tokens up to (not including) a `;` at parenthesis depth 0 (or `stop` keyword).
    fn tokens_until(&mut self, stop_kw: Option<&str>) -> Vec<Token> {
        let mut depth = 0i32;
        let mut out = Vec::new();
        while !self.at_eof() {
            if depth == 0 && (self.is_op(";") || stop_kw.is_some_and(|k| self.is_kw(k))) {
                break;
            }
            if self.is_op("(") {
                depth += 1;
            } else if self.is_op(")") {
                depth -= 1;
            }
            out.push(self.advance());
        }
        out
    }

    fn end_stmt(&mut self) -> SqlResult<()> {
        self.expect_op(";")
    }

    fn loop_end(&mut self, label: &Option<String>) -> SqlResult<()> {
        self.expect_kw("END")?;
        self.expect_kw("LOOP")?;
        if let Some(l) = label {
            if matches!(&self.peek().tok, Tok::Ident(s) if s.eq_ignore_ascii_case(l)) {
                self.advance();
            }
        }
        self.end_stmt()
    }

    fn sub_statement(toks: Vec<Token>) -> SqlResult<Statement> {
        let mut p = Parser::from_tokens(toks);
        let s = p.statement()?;
        if !p.at_eof() {
            return Err(p.unexpected("end of statement"));
        }
        Ok(s)
    }

    fn pl_stmt(&mut self) -> SqlResult<PlStmt> {
        let label = self.opt_label()?;
        let w = self.word().unwrap_or_default();
        match w.as_str() {
            "IF" => {
                self.advance();
                let mut branches = Vec::new();
                let c = self.expr()?;
                self.expect_kw("THEN")?;
                let body = self.pl_stmts(&["ELSIF", "ELSEIF", "ELSE", "END"])?;
                branches.push((c, body));
                let mut else_ = Vec::new();
                loop {
                    if self.eat_kw("ELSIF") || self.eat_kw("ELSEIF") {
                        let c = self.expr()?;
                        self.expect_kw("THEN")?;
                        let body = self.pl_stmts(&["ELSIF", "ELSEIF", "ELSE", "END"])?;
                        branches.push((c, body));
                    } else if self.eat_kw("ELSE") {
                        else_ = self.pl_stmts(&["END"])?;
                    } else {
                        break;
                    }
                }
                self.expect_kw("END")?;
                self.expect_kw("IF")?;
                self.end_stmt()?;
                Ok(PlStmt::If { branches, else_ })
            }
            "WHILE" => {
                self.advance();
                let cond = self.expr()?;
                self.expect_kw("LOOP")?;
                let body = self.pl_stmts(&["END"])?;
                self.loop_end(&label)?;
                Ok(PlStmt::While { label, cond, body })
            }
            "LOOP" => {
                self.advance();
                let body = self.pl_stmts(&["END"])?;
                self.loop_end(&label)?;
                Ok(PlStmt::Loop { label, body })
            }
            "FOR" => {
                self.advance();
                let var = self.ident("loop variable")?.to_ascii_lowercase();
                self.expect_kw("IN")?;
                let is_query = self.is_kw("SELECT") || self.is_kw("WITH") || self.is_kw("VALUES") || (self.is_op("(") && matches!(self.peek_at(1), Tok::Ident(s) if s.eq_ignore_ascii_case("SELECT")));
                if is_query {
                    let toks = self.tokens_until(Some("LOOP"));
                    let query = Self::sub_statement(toks)?;
                    self.expect_kw("LOOP")?;
                    let body = self.pl_stmts(&["END"])?;
                    self.loop_end(&label)?;
                    return Ok(PlStmt::ForQuery { label, var, query: Box::new(query), body });
                }
                let reverse = self.eat_kw("REVERSE");
                let from = self.expr()?;
                self.expect_op("..")?;
                let to = self.expr()?;
                let by = if self.eat_kw("BY") { Some(self.expr()?) } else { None };
                self.expect_kw("LOOP")?;
                let body = self.pl_stmts(&["END"])?;
                self.loop_end(&label)?;
                Ok(PlStmt::ForRange { label, var, reverse, from, to, by, body })
            }
            "FOREACH" => Err(SqlErr::new("0A000", "FOREACH loops are not supported")),
            "EXIT" | "CONTINUE" => {
                self.advance();
                let mut lbl = None;
                if matches!(self.peek().tok, Tok::Ident(_)) && !self.is_kw("WHEN") {
                    lbl = Some(self.any_ident("label")?);
                }
                let when = if self.eat_kw("WHEN") { Some(self.expr()?) } else { None };
                self.end_stmt()?;
                Ok(if w == "EXIT" { PlStmt::Exit { label: lbl, when } } else { PlStmt::Continue { label: lbl, when } })
            }
            "RETURN" => {
                self.advance();
                if self.eat_kw("QUERY") {
                    let toks = self.tokens_until(None);
                    let q = Self::sub_statement(toks)?;
                    self.end_stmt()?;
                    return Ok(PlStmt::ReturnQuery(Box::new(q)));
                }
                if self.eat_kw("NEXT") {
                    let e = if self.is_op(";") { None } else { Some(self.expr()?) };
                    self.end_stmt()?;
                    return Ok(PlStmt::ReturnNext(e));
                }
                let e = if self.is_op(";") { None } else { Some(self.expr()?) };
                self.end_stmt()?;
                Ok(PlStmt::Return(e))
            }
            "RAISE" => {
                self.advance();
                let mut level = None;
                if let Some(l) = self.word().filter(|l| RAISE_LEVELS.contains(&l.as_str())) {
                    self.advance();
                    level = Some(l);
                }
                if self.is_op(";") && level.is_none() {
                    self.advance();
                    return Ok(PlStmt::Raise { level: None, format: String::new(), args: Vec::new(), errcode: None });
                }
                let level = Some(level.unwrap_or_else(|| "EXCEPTION".into()));
                let mut format = String::new();
                let mut errcode = None;
                match &self.peek().tok {
                    Tok::Str(s) => {
                        format = s.clone();
                        self.advance();
                    }
                    Tok::Ident(s) if s.eq_ignore_ascii_case("SQLSTATE") => {
                        self.advance();
                        if let Tok::Str(s) = self.advance().tok {
                            errcode = Some(s);
                        }
                    }
                    Tok::Ident(_) => {
                        let cond = self.ident("condition")?.to_ascii_lowercase();
                        errcode = super::plsql::condition_code(&cond).map(|c| c.to_string());
                        format = cond;
                    }
                    _ => {}
                }
                let mut args = Vec::new();
                while self.eat_op(",") {
                    args.push(self.expr()?);
                }
                if self.eat_kw("USING") {
                    loop {
                        let opt = self.ident("RAISE option")?.to_ascii_uppercase();
                        if !self.eat_op("=") {
                            self.expect_op(":=")?;
                        }
                        let v = self.expr()?;
                        let text = match &v {
                            Expr::Literal(val) => super::eval::text_of(val),
                            _ => String::new(),
                        };
                        match opt.as_str() {
                            "ERRCODE" => errcode = Some(super::plsql::condition_code(&text.to_ascii_lowercase()).map(|c| c.to_string()).unwrap_or(text)),
                            "MESSAGE" => format = text,
                            _ => {}
                        }
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                }
                self.end_stmt()?;
                Ok(PlStmt::Raise { level, format, args, errcode })
            }
            "PERFORM" => {
                let first = self.advance();
                let mut toks = vec![Token { tok: Tok::Ident("SELECT".into()), line: first.line, col: first.col }];
                toks.extend(self.tokens_until(None));
                let q = Self::sub_statement(toks)?;
                self.end_stmt()?;
                Ok(PlStmt::Perform(Box::new(q)))
            }
            "NULL" if matches!(self.peek_at(1), Tok::Op(";")) => {
                self.advance();
                self.end_stmt()?;
                Ok(PlStmt::Null)
            }
            "BEGIN" | "DECLARE" => {
                let b = self.pl_block(label)?;
                self.end_stmt()?;
                Ok(PlStmt::Block(Box::new(b)))
            }
            "GET" => {
                self.advance();
                self.eat_kw("CURRENT");
                self.expect_kw("DIAGNOSTICS")?;
                let mut items = Vec::new();
                loop {
                    let v = self.ident("variable")?.to_ascii_lowercase();
                    if !self.eat_op("=") {
                        self.expect_op(":=")?;
                    }
                    let item = self.ident("diagnostics item")?.to_ascii_uppercase();
                    items.push((v, item));
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.end_stmt()?;
                Ok(PlStmt::GetDiagnostics(items))
            }
            "COMMIT" | "ROLLBACK" | "SAVEPOINT" => Err(SqlErr::new("0A000", "transaction control inside procedures is not supported")),
            _ => {
                // assignment: var := expr / var = expr / rec.field := expr
                if matches!(self.peek().tok, Tok::Ident(_) | Tok::QIdent(_)) {
                    if matches!(self.peek_at(1), Tok::Op(":=") | Tok::Op("=")) {
                        let name = self.ident("variable")?.to_ascii_lowercase();
                        self.advance();
                        let expr = self.expr()?;
                        self.end_stmt()?;
                        return Ok(PlStmt::Assign { name, field: None, expr });
                    }
                    if matches!(self.peek_at(1), Tok::Op(".")) && matches!(self.peek_at(2), Tok::Ident(_)) && matches!(self.peek_at(3), Tok::Op(":=") | Tok::Op("=")) {
                        let name = self.ident("variable")?.to_ascii_lowercase();
                        self.advance();
                        let field = self.ident("field")?.to_ascii_lowercase();
                        self.advance();
                        let expr = self.expr()?;
                        self.end_stmt()?;
                        return Ok(PlStmt::Assign { name, field: Some(field), expr });
                    }
                }
                // an SQL statement, possibly with INTO targets
                let toks = self.tokens_until(None);
                if toks.is_empty() {
                    return Err(self.unexpected("a statement"));
                }
                let (toks, into) = extract_into(toks)?;
                let stmt = Self::sub_statement(toks)?;
                self.end_stmt()?;
                Ok(PlStmt::Sql { stmt: Box::new(stmt), into })
            }
        }
    }
}

fn is_kw(t: &Token, kw: &str) -> bool {
    matches!(&t.tok, Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
}

/// Remove `INTO [STRICT] a, b` from a SELECT (after the select list) or after the
/// `RETURNING` list of INSERT / UPDATE / DELETE.
fn extract_into(toks: Vec<Token>) -> SqlResult<(Vec<Token>, Option<IntoTargets>)> {
    let first = toks.first().map(|t| match &t.tok {
        Tok::Ident(s) => s.to_ascii_uppercase(),
        _ => String::new(),
    });
    let mut depth = 0i32;
    let mut start = None;
    let mut seen_returning = false;
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Op("(") => depth += 1,
            Tok::Op(")") => depth -= 1,
            _ => {}
        }
        if depth != 0 {
            continue;
        }
        if is_kw(t, "RETURNING") {
            seen_returning = true;
        }
        if is_kw(t, "INTO") {
            let ok = match first.as_deref() {
                Some("SELECT") | Some("WITH") => i > 0,
                Some("INSERT") | Some("UPDATE") | Some("DELETE") => seen_returning,
                _ => false,
            };
            if ok {
                start = Some(i);
                break;
            }
        }
    }
    let Some(s) = start else { return Ok((toks, None)) };
    let mut j = s + 1;
    let mut strict = false;
    if toks.get(j).is_some_and(|t| is_kw(t, "STRICT")) {
        strict = true;
        j += 1;
    }
    let mut vars = Vec::new();
    loop {
        match toks.get(j).map(|t| &t.tok) {
            Some(Tok::Ident(n)) | Some(Tok::QIdent(n)) => vars.push(n.to_ascii_lowercase()),
            _ => return Err(SqlErr::syntax("INTO requires variable names")),
        }
        j += 1;
        if matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Op(","))) && matches!(toks.get(j + 1).map(|t| &t.tok), Some(Tok::Ident(_)) | Some(Tok::QIdent(_))) {
            j += 1;
            continue;
        }
        break;
    }
    let mut rest = toks[..s].to_vec();
    rest.extend_from_slice(&toks[j..]);
    Ok((rest, Some(IntoTargets { vars, strict })))
}
