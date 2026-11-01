//! Recursive descent SQL parser.

use camel_core::value::Value;

use super::ast::*;
use super::lexer::{tokenize, Tok, Token};
use super::{SqlErr, SqlResult};

/// Words that end an expression / cannot be used as an implicit alias.
const RESERVED: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "ORDER", "LIMIT", "OFFSET", "HAVING", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "CROSS", "OUTER",
    "ON", "UNION", "AND", "OR", "NOT", "AS", "SET", "VALUES", "RETURNING", "INTO", "FOR", "IS", "IN", "LIKE", "ILIKE", "BETWEEN", "USING",
    "WHEN", "THEN", "ELSE", "END", "ASC", "DESC", "NULLS", "FETCH", "CASE", "DISTINCT", "ALL", "BY", "NULL", "TRUE", "FALSE", "EXISTS",
    "ESCAPE", "DEFAULT",
];

/// Parse a script of one or more `;`-separated statements.
pub fn parse_statements(sql: &str) -> SqlResult<Vec<Statement>> {
    let mut p = Parser::new(sql)?;
    let mut out = Vec::new();
    loop {
        while p.eat_op(";") {}
        if p.at_eof() {
            break;
        }
        out.push(p.statement()?);
        if !p.at_eof() && !p.eat_op(";") {
            return Err(p.unexpected("end of statement"));
        }
    }
    Ok(out)
}

/// Parse exactly one statement.
pub fn parse_statement(sql: &str) -> SqlResult<Statement> {
    let mut v = parse_statements(sql)?;
    match v.len() {
        0 => Err(SqlErr::syntax("empty query")),
        1 => Ok(v.remove(0)),
        n => Err(SqlErr::syntax(format!("expected a single statement but found {}", n))),
    }
}

/// Number of positional (`?`) parameters and highest `$n` used in a script.
pub fn count_parameters(sql: &str) -> SqlResult<usize> {
    let toks = tokenize(sql)?;
    let mut q = 0;
    let mut n = 0;
    for t in toks {
        match t.tok {
            Tok::Param => q += 1,
            Tok::NumParam(k) => n = n.max(k),
            _ => {}
        }
    }
    Ok(q.max(n))
}

pub(crate) struct Parser {
    pub(crate) toks: Vec<Token>,
    pub(crate) pos: usize,
    pub(crate) next_param: usize,
}

impl Parser {
    pub(crate) fn new(sql: &str) -> SqlResult<Self> {
        Ok(Parser { toks: tokenize(sql)?, pos: 0, next_param: 0 })
    }

    pub(crate) fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }
    pub(crate) fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    pub(crate) fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.peek().tok, Tok::Eof)
    }

    pub(crate) fn unexpected(&self, expected: &str) -> SqlErr {
        let t = self.peek();
        let near = if matches!(t.tok, Tok::Eof) { "syntax error at end of input".to_string() } else { format!("syntax error at or near \"{}\"", t.describe()) };
        SqlErr::syntax(format!("{} (line {}, column {}): expected {}", near, t.line, t.col, expected))
    }

    pub(crate) fn is_kw(&self, kw: &str) -> bool {
        matches!(&self.peek().tok, Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    pub(crate) fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    pub(crate) fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }
    pub(crate) fn expect_kw(&mut self, kw: &str) -> SqlResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.unexpected(kw))
        }
    }
    pub(crate) fn is_op(&self, op: &str) -> bool {
        matches!(&self.peek().tok, Tok::Op(o) if *o == op)
    }
    pub(crate) fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.advance();
            true
        } else {
            false
        }
    }
    pub(crate) fn expect_op(&mut self, op: &str) -> SqlResult<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("\"{}\"", op)))
        }
    }

    /// An identifier (quoted or not, keywords not reserved allowed).
    pub(crate) fn ident(&mut self, what: &str) -> SqlResult<String> {
        match &self.peek().tok {
            Tok::Ident(s) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s)) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            Tok::QIdent(s) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    /// A possibly schema-qualified table name (`public.orders` → `orders`).
    pub(crate) fn table_name(&mut self) -> SqlResult<String> {
        let mut name = self.ident("table name")?;
        while self.is_op(".") {
            self.advance();
            name = self.ident("table name")?;
        }
        Ok(name)
    }

    pub(crate) fn statement(&mut self) -> SqlResult<Statement> {
        let t = self.peek().clone();
        let word = match &t.tok {
            Tok::Ident(s) => s.to_ascii_uppercase(),
            Tok::Op("(") => return Ok(Statement::Select(Box::new(self.select()?))),
            _ => return Err(self.unexpected("a statement (SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, BEGIN, COMMIT, ROLLBACK)")),
        };
        match word.as_str() {
            "SELECT" | "WITH" | "VALUES" => Ok(Statement::Select(Box::new(self.select()?))),
            "INSERT" => self.insert(),
            "UPDATE" => self.update(),
            "DELETE" => self.delete(),
            "CREATE" => self.create(),
            "DROP" => self.drop(),
            "CALL" | "EXEC" | "EXECUTE" => self.call_stmt(),
            "DO" => self.do_stmt(),
            "TRUNCATE" => {
                self.advance();
                self.eat_kw("TABLE");
                let name = self.table_name()?;
                Ok(Statement::Truncate { name })
            }
            "ALTER" if matches!(self.peek_at(1), Tok::Ident(ref w) if w.eq_ignore_ascii_case("TABLE")) => self.alter_table(),
            "ALTER" | "SET" | "COMMENT" | "ANALYZE" | "VACUUM" | "GRANT" => {
                self.advance();
                while !self.at_eof() && !self.is_op(";") {
                    self.advance();
                }
                Ok(Statement::NoOp { command: word })
            }
            "BEGIN" | "START" => {
                self.advance();
                if word == "START" {
                    self.expect_kw("TRANSACTION")?;
                } else if !self.eat_kw("TRANSACTION") {
                    self.eat_kw("WORK");
                }
                self.skip_transaction_modes();
                Ok(Statement::Begin)
            }
            "COMMIT" | "END" => {
                self.advance();
                if !self.eat_kw("TRANSACTION") {
                    self.eat_kw("WORK");
                }
                Ok(Statement::Commit)
            }
            "ROLLBACK" | "ABORT" => {
                self.advance();
                if !self.eat_kw("TRANSACTION") {
                    self.eat_kw("WORK");
                }
                Ok(Statement::Rollback)
            }
            _ => Err(SqlErr::syntax(format!(
                "syntax error at or near \"{}\" (line {}, column {}): expected a statement (SELECT, INSERT, UPDATE, DELETE, CREATE, DROP, BEGIN, COMMIT, ROLLBACK)",
                t.describe(),
                t.line,
                t.col
            ))),
        }
    }

    pub(crate) fn skip_transaction_modes(&mut self) {
        // ISOLATION LEVEL ..., READ WRITE, READ ONLY
        while !self.at_eof() && !self.is_op(";") {
            self.advance();
        }
    }

    // ------------------------------------------------------------------ DDL

    pub(crate) fn create(&mut self) -> SqlResult<Statement> {
        self.expect_kw("CREATE")?;
        let or_replace = self.eat_kw("OR");
        self.eat_kw("REPLACE");
        if self.is_kw("PROCEDURE") || self.is_kw("FUNCTION") {
            return self.create_routine(or_replace);
        }
        let unique = self.eat_kw("UNIQUE");
        if self.is_kw("INDEX") || unique || self.is_kw("SEQUENCE") || self.is_kw("VIEW") {
            let what = self.advance().describe().to_ascii_uppercase();
            while !self.at_eof() && !self.is_op(";") {
                self.advance();
            }
            return Ok(Statement::NoOp { command: format!("CREATE {}", what) });
        }
        self.eat_kw("TEMPORARY");
        self.eat_kw("TEMP");
        self.eat_kw("MEMORY");
        self.eat_kw("CACHED");
        self.expect_kw("TABLE")?;
        let mut if_not_exists = false;
        if self.eat_kw("IF") {
            self.expect_kw("NOT")?;
            self.expect_kw("EXISTS")?;
            if_not_exists = true;
        }
        let name = self.table_name()?;
        self.expect_op("(")?;
        let mut columns = Vec::new();
        let mut constraints = Vec::new();
        loop {
            if self.eat_kw("CONSTRAINT") {
                self.ident("constraint name")?;
            }
            if self.is_kw("PRIMARY") {
                self.advance();
                self.expect_kw("KEY")?;
                constraints.push(TableConstraint::PrimaryKey(self.ident_list()?));
            } else if self.is_kw("UNIQUE") {
                self.advance();
                self.eat_kw("KEY");
                constraints.push(TableConstraint::Unique(self.ident_list()?));
            } else if self.is_kw("FOREIGN") || self.is_kw("CHECK") || (self.is_kw("KEY") || self.is_kw("INDEX")) && !matches!(self.peek_at(1), Tok::Ident(_)) {
                self.skip_constraint()?;
            } else {
                columns.push(self.column_def()?);
            }
            if self.eat_op(",") {
                continue;
            }
            self.expect_op(")")?;
            break;
        }
        // table options (ENGINE=..., etc.) are ignored
        while !self.at_eof() && !self.is_op(";") {
            self.advance();
        }
        if columns.is_empty() {
            return Err(SqlErr::syntax(format!("table \"{}\" must have at least one column", name)));
        }
        Ok(Statement::CreateTable { name, if_not_exists, columns, constraints })
    }

    pub(crate) fn skip_constraint(&mut self) -> SqlResult<()> {
        let mut depth = 0;
        loop {
            if self.at_eof() {
                return Err(self.unexpected("\")\""));
            }
            if depth == 0 && (self.is_op(",") || self.is_op(")")) {
                return Ok(());
            }
            if self.is_op("(") {
                depth += 1;
            } else if self.is_op(")") {
                depth -= 1;
            }
            self.advance();
        }
    }

    pub(crate) fn ident_list(&mut self) -> SqlResult<Vec<String>> {
        self.expect_op("(")?;
        let mut v = vec![self.ident("column name")?];
        while self.eat_op(",") {
            v.push(self.ident("column name")?);
        }
        self.expect_op(")")?;
        Ok(v)
    }

    pub(crate) fn data_type(&mut self) -> SqlResult<(DataType, bool)> {
        let t = self.peek().clone();
        let word = match &t.tok {
            Tok::Ident(s) => s.to_ascii_uppercase(),
            _ => return Err(self.unexpected("a data type")),
        };
        self.advance();
        let mut serial = false;
        let ty = match word.as_str() {
            "INT" | "INTEGER" | "INT4" | "SMALLINT" | "INT2" | "TINYINT" | "MEDIUMINT" => DataType::Integer,
            "BIGINT" | "INT8" | "LONG" => DataType::BigInt,
            "SERIAL" | "SERIAL4" | "SMALLSERIAL" | "SERIAL2" => {
                serial = true;
                DataType::Integer
            }
            "BIGSERIAL" | "SERIAL8" => {
                serial = true;
                DataType::BigInt
            }
            "VARCHAR" | "VARCHAR2" | "NVARCHAR" | "CHAR" | "NCHAR" | "CHARACTER" | "VARCHAR_IGNORECASE" => {
                self.eat_kw("VARYING");
                DataType::Varchar(self.type_len()?.0.map(|n| n as usize))
            }
            "TEXT" | "CLOB" | "LONGTEXT" | "MEDIUMTEXT" | "STRING" | "JSON" | "JSONB" | "UUID" | "XML" | "NAME" => DataType::Text,
            "BOOLEAN" | "BOOL" | "BIT" => {
                self.type_len()?;
                DataType::Boolean
            }
            "DOUBLE" => {
                self.eat_kw("PRECISION");
                DataType::Double
            }
            "FLOAT" | "FLOAT8" | "REAL" | "FLOAT4" => {
                self.type_len()?;
                DataType::Double
            }
            "DECIMAL" | "NUMERIC" | "DEC" | "NUMBER" => {
                let (p, s) = self.type_len()?;
                DataType::Decimal(p, s)
            }
            "TIMESTAMP" | "DATETIME" | "TIMESTAMPTZ" | "TIME" | "TIMETZ" => {
                self.type_len()?;
                if self.eat_kw("WITH") || self.eat_kw("WITHOUT") {
                    self.expect_kw("TIME")?;
                    self.expect_kw("ZONE")?;
                }
                DataType::Timestamp
            }
            "DATE" => DataType::Date,
            "BYTEA" | "BLOB" | "BINARY" | "VARBINARY" | "LONGBLOB" => {
                self.type_len()?;
                DataType::Bytes
            }
            _ => {
                return Err(SqlErr::syntax(format!("type \"{}\" does not exist (line {}, column {})", t.describe(), t.line, t.col)));
            }
        };
        Ok((ty, serial))
    }

    pub(crate) fn type_len(&mut self) -> SqlResult<(Option<u32>, Option<u32>)> {
        if !self.eat_op("(") {
            return Ok((None, None));
        }
        let mut nums = Vec::new();
        loop {
            match self.advance().tok {
                Tok::Int(n) if n >= 0 => nums.push(n as u32),
                Tok::Ident(s) if s.eq_ignore_ascii_case("MAX") => nums.push(u32::MAX),
                _ => return Err(SqlErr::syntax("invalid type length")),
            }
            if self.eat_op(",") {
                continue;
            }
            self.expect_op(")")?;
            break;
        }
        Ok((nums.first().copied().filter(|n| *n != u32::MAX), nums.get(1).copied()))
    }

    pub(crate) fn column_def(&mut self) -> SqlResult<ColumnDef> {
        let name = self.ident("column name")?;
        let (ty, serial) = self.data_type()?;
        let mut c = ColumnDef { name, ty, not_null: false, primary_key: false, unique: false, default: None, auto_increment: serial };
        loop {
            if self.eat_kw("NOT") {
                self.expect_kw("NULL")?;
                c.not_null = true;
            } else if self.eat_kw("NULL") {
            } else if self.eat_kw("PRIMARY") {
                self.expect_kw("KEY")?;
                c.primary_key = true;
                c.not_null = true;
            } else if self.eat_kw("UNIQUE") {
                c.unique = true;
            } else if self.eat_kw("DEFAULT") {
                c.default = Some(self.concat_expr()?);
            } else if self.eat_kw("AUTO_INCREMENT") || self.eat_kw("AUTOINCREMENT") || self.eat_kw("IDENTITY") {
                c.auto_increment = true;
                if self.is_op("(") {
                    self.type_len()?;
                }
            } else if self.eat_kw("GENERATED") {
                if !self.eat_kw("ALWAYS") {
                    self.expect_kw("BY")?;
                    self.expect_kw("DEFAULT")?;
                }
                self.expect_kw("AS")?;
                self.expect_kw("IDENTITY")?;
                if self.is_op("(") {
                    self.skip_parens()?;
                }
                c.auto_increment = true;
            } else if self.eat_kw("REFERENCES") {
                self.table_name()?;
                if self.is_op("(") {
                    self.skip_parens()?;
                }
                while self.eat_kw("ON") {
                    self.advance();
                    self.advance();
                    if self.is_kw("NULL") || self.is_kw("DEFAULT") || self.is_kw("ACTION") {
                        self.advance();
                    }
                }
            } else if self.eat_kw("CHECK") {
                self.skip_parens()?;
            } else if self.eat_kw("CONSTRAINT") {
                self.ident("constraint name")?;
            } else if self.eat_kw("COLLATE") {
                self.advance();
            } else {
                break;
            }
        }
        Ok(c)
    }

    pub(crate) fn skip_parens(&mut self) -> SqlResult<()> {
        self.expect_op("(")?;
        let mut depth = 1;
        while depth > 0 {
            if self.at_eof() {
                return Err(self.unexpected("\")\""));
            }
            if self.is_op("(") {
                depth += 1;
            } else if self.is_op(")") {
                depth -= 1;
            }
            self.advance();
        }
        Ok(())
    }

    pub(crate) fn drop(&mut self) -> SqlResult<Statement> {
        self.expect_kw("DROP")?;
        if self.is_kw("PROCEDURE") || self.is_kw("FUNCTION") {
            let function = self.advance().describe().eq_ignore_ascii_case("FUNCTION");
            let mut if_exists = false;
            if self.eat_kw("IF") {
                self.expect_kw("EXISTS")?;
                if_exists = true;
            }
            let name = self.table_name()?.to_ascii_lowercase();
            if self.is_op("(") {
                self.skip_parens()?;
            }
            if !self.eat_kw("CASCADE") {
                self.eat_kw("RESTRICT");
            }
            return Ok(Statement::DropRoutine { name, function, if_exists });
        }
        if !self.is_kw("TABLE") {
            let what = self.advance().describe().to_ascii_uppercase();
            while !self.at_eof() && !self.is_op(";") {
                self.advance();
            }
            return Ok(Statement::NoOp { command: format!("DROP {}", what) });
        }
        self.advance();
        let mut if_exists = false;
        if self.eat_kw("IF") {
            self.expect_kw("EXISTS")?;
            if_exists = true;
        }
        let mut names = vec![self.table_name()?];
        while self.eat_op(",") {
            names.push(self.table_name()?);
        }
        if !self.eat_kw("CASCADE") {
            self.eat_kw("RESTRICT");
        }
        Ok(Statement::DropTable { names, if_exists })
    }

    // ------------------------------------------------------------------ DML

    pub(crate) fn insert(&mut self) -> SqlResult<Statement> {
        self.expect_kw("INSERT")?;
        self.expect_kw("INTO")?;
        let table = self.table_name()?;
        let mut columns = None;
        if self.is_op("(") && !(self.is_kw_at(1, "SELECT") || self.is_kw_at(1, "VALUES")) {
            columns = Some(self.ident_list()?);
        }
        let source = if self.eat_kw("VALUES") {
            let mut rows = Vec::new();
            loop {
                self.expect_op("(")?;
                let mut row = vec![self.insert_value()?];
                while self.eat_op(",") {
                    row.push(self.insert_value()?);
                }
                self.expect_op(")")?;
                rows.push(row);
                if !self.eat_op(",") {
                    break;
                }
            }
            InsertSource::Values(rows)
        } else if self.eat_kw("DEFAULT") {
            self.expect_kw("VALUES")?;
            InsertSource::DefaultValues
        } else if self.is_kw("SELECT") || self.is_op("(") {
            InsertSource::Select(Box::new(self.select()?))
        } else {
            return Err(self.unexpected("VALUES or SELECT"));
        };
        let returning = self.returning()?;
        Ok(Statement::Insert { table, columns, source, returning })
    }

    pub(crate) fn insert_value(&mut self) -> SqlResult<Expr> {
        if self.is_kw("DEFAULT") {
            self.advance();
            return Ok(Expr::Function { name: "DEFAULT".into(), args: vec![], distinct: false, star: false });
        }
        self.expr()
    }

    pub(crate) fn returning(&mut self) -> SqlResult<Option<Vec<SelectItem>>> {
        if self.eat_kw("RETURNING") {
            Ok(Some(self.select_items()?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn opt_alias(&mut self) -> SqlResult<Option<String>> {
        if self.eat_kw("AS") {
            return Ok(Some(self.ident("alias")?));
        }
        match &self.peek().tok {
            Tok::Ident(s) if !RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s)) => Ok(Some(self.ident("alias")?)),
            Tok::QIdent(_) => Ok(Some(self.ident("alias")?)),
            _ => Ok(None),
        }
    }

    pub(crate) fn update(&mut self) -> SqlResult<Statement> {
        self.expect_kw("UPDATE")?;
        let table = self.table_name()?;
        let alias = if self.is_kw("SET") { None } else { self.opt_alias()? };
        self.expect_kw("SET")?;
        let mut sets = Vec::new();
        loop {
            let mut col = self.ident("column name")?;
            if self.eat_op(".") {
                col = self.ident("column name")?;
            }
            self.expect_op("=")?;
            let e = if self.is_kw("DEFAULT") {
                self.advance();
                Expr::Function { name: "DEFAULT".into(), args: vec![], distinct: false, star: false }
            } else {
                self.expr()?
            };
            sets.push((col, e));
            if !self.eat_op(",") {
                break;
            }
        }
        let where_ = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        let returning = self.returning()?;
        Ok(Statement::Update { table, alias, sets, where_, returning })
    }

    pub(crate) fn delete(&mut self) -> SqlResult<Statement> {
        self.expect_kw("DELETE")?;
        self.expect_kw("FROM")?;
        let table = self.table_name()?;
        let alias = if self.is_kw("WHERE") || self.is_kw("RETURNING") { None } else { self.opt_alias()? };
        let where_ = if self.eat_kw("WHERE") { Some(self.expr()?) } else { None };
        let returning = self.returning()?;
        Ok(Statement::Delete { table, alias, where_, returning })
    }

    // ------------------------------------------------------------------ SELECT

    pub(crate) fn alter_table(&mut self) -> SqlResult<Statement> {
        self.expect_kw("ALTER")?;
        self.expect_kw("TABLE")?;
        let mut if_exists = false;
        if self.eat_kw("IF") {
            self.expect_kw("EXISTS")?;
            if_exists = true;
        }
        self.eat_kw("ONLY");
        let name = self.table_name()?;
        let action = if self.eat_kw("ADD") {
            if self.is_kw("CONSTRAINT") || self.is_kw("PRIMARY") || self.is_kw("UNIQUE") || self.is_kw("FOREIGN") || self.is_kw("CHECK") {
                let cname = if self.eat_kw("CONSTRAINT") { Some(self.ident("constraint name")?) } else { None };
                if self.eat_kw("PRIMARY") {
                    self.expect_kw("KEY")?;
                    AlterAction::AddConstraint { name: cname, constraint: TableConstraint::PrimaryKey(self.ident_list()?) }
                } else if self.eat_kw("UNIQUE") {
                    AlterAction::AddConstraint { name: cname, constraint: TableConstraint::Unique(self.ident_list()?) }
                } else {
                    // FOREIGN KEY / CHECK: accepted, not enforced
                    while !self.at_eof() && !self.is_op(";") {
                        self.advance();
                    }
                    return Ok(Statement::NoOp { command: "ALTER TABLE".into() });
                }
            } else {
                self.eat_kw("COLUMN");
                let mut if_not_exists = false;
                if self.eat_kw("IF") {
                    self.expect_kw("NOT")?;
                    self.expect_kw("EXISTS")?;
                    if_not_exists = true;
                }
                AlterAction::AddColumn { def: self.column_def()?, if_not_exists }
            }
        } else if self.eat_kw("DROP") {
            if self.eat_kw("CONSTRAINT") {
                let mut ie = false;
                if self.eat_kw("IF") {
                    self.expect_kw("EXISTS")?;
                    ie = true;
                }
                AlterAction::DropConstraint { name: self.ident("constraint name")?, if_exists: ie }
            } else {
                self.eat_kw("COLUMN");
                let mut ie = false;
                if self.eat_kw("IF") {
                    self.expect_kw("EXISTS")?;
                    ie = true;
                }
                let col = self.ident("column name")?;
                if !self.eat_kw("CASCADE") {
                    self.eat_kw("RESTRICT");
                }
                AlterAction::DropColumn { name: col, if_exists: ie }
            }
        } else if self.eat_kw("RENAME") {
            if self.eat_kw("TO") {
                AlterAction::RenameTable { to: self.ident("table name")? }
            } else {
                self.eat_kw("COLUMN");
                let from = self.ident("column name")?;
                self.expect_kw("TO")?;
                AlterAction::RenameColumn { from, to: self.ident("column name")? }
            }
        } else if self.eat_kw("ALTER") || self.eat_kw("MODIFY") {
            self.eat_kw("COLUMN");
            let column = self.ident("column name")?;
            if self.eat_kw("SET") {
                if self.eat_kw("DEFAULT") {
                    AlterAction::SetDefault { column, default: Some(self.expr()?) }
                } else if self.eat_kw("NOT") {
                    self.expect_kw("NULL")?;
                    AlterAction::SetNotNull { column, not_null: true }
                } else {
                    self.expect_kw("DATA")?;
                    self.expect_kw("TYPE")?;
                    AlterAction::SetType { column, ty: self.data_type()?.0 }
                }
            } else if self.eat_kw("DROP") {
                if self.eat_kw("DEFAULT") {
                    AlterAction::SetDefault { column, default: None }
                } else {
                    self.expect_kw("NOT")?;
                    self.expect_kw("NULL")?;
                    AlterAction::SetNotNull { column, not_null: false }
                }
            } else {
                self.eat_kw("TYPE");
                let ty = self.data_type()?.0;
                if self.eat_kw("USING") {
                    self.expr()?;
                }
                AlterAction::SetType { column, ty }
            }
        } else {
            return Err(self.unexpected("ADD, DROP, RENAME or ALTER"));
        };
        Ok(Statement::AlterTable { name, if_exists, action })
    }

    pub(crate) fn select(&mut self) -> SqlResult<Select> {
        if self.eat_kw("WITH") {
            let recursive = self.eat_kw("RECURSIVE");
            let mut ctes = Vec::new();
            loop {
                let name = self.ident("CTE name")?;
                let columns = if self.is_op("(") { Some(self.ident_list()?) } else { None };
                self.expect_kw("AS")?;
                if self.eat_kw("NOT") {
                    self.expect_kw("MATERIALIZED")?;
                } else {
                    self.eat_kw("MATERIALIZED");
                }
                self.expect_op("(")?;
                let query = self.select()?;
                self.expect_op(")")?;
                ctes.push(Cte { name, columns, query: Box::new(query), recursive });
                if !self.eat_op(",") {
                    break;
                }
            }
            let mut main = self.select()?;
            ctes.extend(std::mem::take(&mut main.ctes));
            main.ctes = ctes;
            return Ok(main);
        }
        let mut s = self.select_core()?;
        loop {
            if self.eat_kw("UNION") {
                let all = self.eat_kw("ALL");
                if !all {
                    self.eat_kw("DISTINCT");
                }
                let part = self.select_core()?;
                s.unions.push((all, part));
            } else {
                break;
            }
        }
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let expr = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                let mut nulls_first = None;
                if self.eat_kw("NULLS") {
                    if self.eat_kw("FIRST") {
                        nulls_first = Some(true);
                    } else {
                        self.expect_kw("LAST")?;
                        nulls_first = Some(false);
                    }
                }
                s.order_by.push(OrderItem { expr, desc, nulls_first });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        loop {
            if self.eat_kw("LIMIT") {
                if self.eat_kw("ALL") {
                    continue;
                }
                let first = self.expr()?;
                if self.eat_op(",") {
                    s.offset = Some(first);
                    s.limit = Some(self.expr()?);
                } else {
                    s.limit = Some(first);
                }
            } else if self.eat_kw("OFFSET") {
                s.offset = Some(self.expr()?);
                if !self.eat_kw("ROWS") {
                    self.eat_kw("ROW");
                }
            } else if self.eat_kw("FETCH") {
                if !self.eat_kw("FIRST") {
                    self.expect_kw("NEXT")?;
                }
                s.limit = Some(if self.is_kw("ROW") || self.is_kw("ROWS") { Expr::Literal(Value::Int(1)) } else { self.expr()? });
                if !self.eat_kw("ROWS") {
                    self.expect_kw("ROW")?;
                }
                self.expect_kw("ONLY")?;
            } else if self.eat_kw("FOR") {
                // FOR UPDATE [SKIP LOCKED | NOWAIT] — locking is a no-op
                if !self.eat_kw("UPDATE") {
                    self.expect_kw("SHARE")?;
                }
                if self.eat_kw("SKIP") {
                    self.expect_kw("LOCKED")?;
                }
                self.eat_kw("NOWAIT");
            } else {
                break;
            }
        }
        Ok(s)
    }

    pub(crate) fn select_core(&mut self) -> SqlResult<Select> {
        if self.eat_op("(") {
            let s = self.select()?;
            self.expect_op(")")?;
            return Ok(s);
        }
        if self.eat_kw("VALUES") {
            // VALUES (..), (..) as a query: column1, column2...
            let mut rows: Vec<Vec<Expr>> = Vec::new();
            loop {
                self.expect_op("(")?;
                let mut row = vec![self.expr()?];
                while self.eat_op(",") {
                    row.push(self.expr()?);
                }
                self.expect_op(")")?;
                rows.push(row);
                if !self.eat_op(",") {
                    break;
                }
            }
            let mut first: Option<Select> = None;
            for row in rows {
                let items = row.into_iter().enumerate().map(|(i, e)| SelectItem::Expr { expr: e, alias: Some(format!("column{}", i + 1)) }).collect();
                let s = Select { items, ..Default::default() };
                match &mut first {
                    None => first = Some(s),
                    Some(f) => f.unions.push((true, s)),
                }
            }
            return Ok(first.unwrap_or_default());
        }
        self.expect_kw("SELECT")?;
        let mut s = Select::default();
        if self.eat_kw("DISTINCT") {
            s.distinct = true;
        } else {
            self.eat_kw("ALL");
        }
        if self.eat_kw("TOP") {
            s.limit = Some(self.primary()?);
        }
        s.items = self.select_items()?;
        if self.eat_kw("FROM") {
            let base = self.table_ref()?;
            let mut joins = Vec::new();
            loop {
                if self.eat_op(",") {
                    joins.push(Join { kind: JoinKind::Cross, table: self.table_ref()?, on: None });
                    continue;
                }
                let kind = if self.eat_kw("JOIN") {
                    JoinKind::Inner
                } else if self.is_kw("INNER") {
                    self.advance();
                    self.expect_kw("JOIN")?;
                    JoinKind::Inner
                } else if self.is_kw("LEFT") || self.is_kw("RIGHT") {
                    let left = self.is_kw("LEFT");
                    self.advance();
                    self.eat_kw("OUTER");
                    self.expect_kw("JOIN")?;
                    if left {
                        JoinKind::Left
                    } else {
                        JoinKind::Right
                    }
                } else if self.is_kw("CROSS") {
                    self.advance();
                    self.expect_kw("JOIN")?;
                    JoinKind::Cross
                } else if self.is_kw("FULL") {
                    self.advance();
                    self.eat_kw("OUTER");
                    self.expect_kw("JOIN")?;
                    JoinKind::Full
                } else {
                    break;
                };
                let table = self.table_ref()?;
                let on = if kind == JoinKind::Cross {
                    None
                } else if self.eat_kw("ON") {
                    Some(self.expr()?)
                } else if self.eat_kw("USING") {
                    let cols = self.ident_list()?;
                    let lname = table_ref_name(&base, &joins);
                    let rname = match &table {
                        TableRef::Table { name, alias } => alias.clone().unwrap_or(name.clone()),
                        TableRef::Subquery { alias, .. } => alias.clone(),
                        TableRef::Function { name, alias, .. } | TableRef::Values { name, alias, .. } => alias.clone().unwrap_or(name.to_ascii_lowercase()),
                    };
                    let mut cond: Option<Expr> = None;
                    for c in cols {
                        let e = Expr::Binary {
                            op: BinOp::Eq,
                            left: Box::new(Expr::Column { table: Some(lname.clone()), name: c.clone() }),
                            right: Box::new(Expr::Column { table: Some(rname.clone()), name: c }),
                        };
                        cond = Some(match cond {
                            None => e,
                            Some(prev) => Expr::Binary { op: BinOp::And, left: Box::new(prev), right: Box::new(e) },
                        });
                    }
                    cond
                } else {
                    return Err(self.unexpected("ON"));
                };
                joins.push(Join { kind, table, on });
            }
            s.from = Some(From { base, joins });
        }
        if self.eat_kw("WHERE") {
            s.where_ = Some(self.expr()?);
        }
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            s.group_by.push(self.expr()?);
            while self.eat_op(",") {
                s.group_by.push(self.expr()?);
            }
        }
        if self.eat_kw("HAVING") {
            s.having = Some(self.expr()?);
        }
        Ok(s)
    }

    pub(crate) fn table_ref(&mut self) -> SqlResult<TableRef> {
        if self.eat_op("(") {
            let q = self.select()?;
            self.expect_op(")")?;
            let alias = self.opt_alias()?.ok_or_else(|| SqlErr::syntax("subquery in FROM must have an alias"))?;
            return Ok(TableRef::Subquery { query: Box::new(q), alias });
        }
        let name = self.table_name()?;
        if self.eat_op("(") {
            // set-returning function
            let mut args = Vec::new();
            if !self.eat_op(")") {
                loop {
                    args.push(self.expr()?);
                    if self.eat_op(")") {
                        break;
                    }
                    self.expect_op(",")?;
                }
            }
            let alias = self.opt_alias()?;
            return Ok(TableRef::Function { name: name.to_ascii_lowercase(), args, alias });
        }
        let alias = self.opt_alias()?;
        Ok(TableRef::Table { name, alias })
    }

    pub(crate) fn select_items(&mut self) -> SqlResult<Vec<SelectItem>> {
        let mut items = Vec::new();
        loop {
            if self.eat_op("*") {
                items.push(SelectItem::Wildcard);
            } else if matches!(self.peek().tok, Tok::Ident(_) | Tok::QIdent(_)) && matches!(self.peek_at(1), Tok::Op(".")) && matches!(self.peek_at(2), Tok::Op("*")) {
                let t = self.ident("table name")?;
                self.advance();
                self.advance();
                items.push(SelectItem::QualifiedWildcard(t));
            } else {
                let expr = self.expr()?;
                let alias = self.opt_alias()?;
                items.push(SelectItem::Expr { expr, alias });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(items)
    }

    // ------------------------------------------------------------------ expressions

    pub(crate) fn expr(&mut self) -> SqlResult<Expr> {
        self.or_expr()
    }

    pub(crate) fn or_expr(&mut self) -> SqlResult<Expr> {
        let mut l = self.and_expr()?;
        while self.eat_kw("OR") {
            let r = self.and_expr()?;
            l = Expr::Binary { op: BinOp::Or, left: Box::new(l), right: Box::new(r) };
        }
        Ok(l)
    }

    pub(crate) fn and_expr(&mut self) -> SqlResult<Expr> {
        let mut l = self.not_expr()?;
        while self.eat_kw("AND") {
            let r = self.not_expr()?;
            l = Expr::Binary { op: BinOp::And, left: Box::new(l), right: Box::new(r) };
        }
        Ok(l)
    }

    pub(crate) fn not_expr(&mut self) -> SqlResult<Expr> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Not(Box::new(self.not_expr()?)));
        }
        self.predicate()
    }

    pub(crate) fn predicate(&mut self) -> SqlResult<Expr> {
        let left = self.concat_expr()?;
        let op = match &self.peek().tok {
            Tok::Op("=") => Some(BinOp::Eq),
            Tok::Op("<>") | Tok::Op("!=") => Some(BinOp::NotEq),
            Tok::Op("<") => Some(BinOp::Lt),
            Tok::Op("<=") => Some(BinOp::LtEq),
            Tok::Op(">") => Some(BinOp::Gt),
            Tok::Op(">=") => Some(BinOp::GtEq),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let right = self.concat_expr()?;
            return Ok(Expr::Binary { op, left: Box::new(left), right: Box::new(right) });
        }
        if self.eat_kw("IS") {
            let negated = self.eat_kw("NOT");
            if self.eat_kw("NULL") {
                return Ok(Expr::IsNull { expr: Box::new(left), negated });
            }
            if self.eat_kw("TRUE") {
                return Ok(Expr::IsBool { expr: Box::new(left), value: true, negated });
            }
            if self.eat_kw("FALSE") {
                return Ok(Expr::IsBool { expr: Box::new(left), value: false, negated });
            }
            if self.eat_kw("DISTINCT") {
                self.expect_kw("FROM")?;
                let right = self.concat_expr()?;
                // a IS DISTINCT FROM b  ==  NOT (a <=> b)
                let f = Expr::Function { name: "__NOT_DISTINCT".into(), args: vec![left, right], distinct: false, star: false };
                return Ok(if negated { f } else { Expr::Not(Box::new(f)) });
            }
            return Err(self.unexpected("NULL, TRUE, FALSE or DISTINCT FROM"));
        }
        let negated = if self.is_kw("NOT") && (self.is_kw_at(1, "LIKE") || self.is_kw_at(1, "ILIKE") || self.is_kw_at(1, "IN") || self.is_kw_at(1, "BETWEEN")) {
            self.advance();
            true
        } else {
            false
        };
        if self.is_kw("LIKE") || self.is_kw("ILIKE") {
            let ci = self.is_kw("ILIKE");
            self.advance();
            let pattern = self.concat_expr()?;
            let escape = if self.eat_kw("ESCAPE") {
                match self.advance().tok {
                    Tok::Str(s) if s.chars().count() <= 1 => s.chars().next(),
                    _ => return Err(SqlErr::syntax("ESCAPE must be followed by a single character string")),
                }
            } else {
                Some('\\')
            };
            return Ok(Expr::Like { expr: Box::new(left), pattern: Box::new(pattern), negated, case_insensitive: ci, escape });
        }
        if self.eat_kw("IN") {
            self.expect_op("(")?;
            if self.is_kw("SELECT") || self.is_kw("WITH") {
                let q = self.select()?;
                self.expect_op(")")?;
                return Ok(Expr::InSubquery { expr: Box::new(left), query: Box::new(q), negated });
            }
            let mut list = Vec::new();
            if !self.is_op(")") {
                list.push(self.expr()?);
                while self.eat_op(",") {
                    list.push(self.expr()?);
                }
            }
            self.expect_op(")")?;
            return Ok(Expr::InList { expr: Box::new(left), list, negated });
        }
        if self.eat_kw("BETWEEN") {
            self.eat_kw("SYMMETRIC");
            let low = self.concat_expr()?;
            self.expect_kw("AND")?;
            let high = self.concat_expr()?;
            return Ok(Expr::Between { expr: Box::new(left), low: Box::new(low), high: Box::new(high), negated });
        }
        if negated {
            return Err(self.unexpected("LIKE, IN or BETWEEN"));
        }
        Ok(left)
    }

    pub(crate) fn concat_expr(&mut self) -> SqlResult<Expr> {
        let mut l = self.additive()?;
        while self.eat_op("||") {
            let r = self.additive()?;
            l = Expr::Binary { op: BinOp::Concat, left: Box::new(l), right: Box::new(r) };
        }
        Ok(l)
    }

    pub(crate) fn additive(&mut self) -> SqlResult<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            let op = if self.is_op("+") {
                BinOp::Add
            } else if self.is_op("-") {
                BinOp::Sub
            } else {
                break;
            };
            self.advance();
            let r = self.multiplicative()?;
            l = Expr::Binary { op, left: Box::new(l), right: Box::new(r) };
        }
        Ok(l)
    }

    pub(crate) fn multiplicative(&mut self) -> SqlResult<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = if self.is_op("*") {
                BinOp::Mul
            } else if self.is_op("/") {
                BinOp::Div
            } else if self.is_op("%") {
                BinOp::Mod
            } else {
                break;
            };
            self.advance();
            let r = self.unary()?;
            l = Expr::Binary { op, left: Box::new(l), right: Box::new(r) };
        }
        Ok(l)
    }

    pub(crate) fn unary(&mut self) -> SqlResult<Expr> {
        if self.eat_op("-") {
            let e = self.unary()?;
            return Ok(match e {
                Expr::Literal(Value::Int(i)) => Expr::Literal(Value::Int(-i)),
                Expr::Literal(Value::Float(f)) => Expr::Literal(Value::Float(-f)),
                e => Expr::Neg(Box::new(e)),
            });
        }
        if self.eat_op("+") {
            return self.unary();
        }
        let mut e = self.primary()?;
        while self.eat_op("::") {
            let (ty, _) = self.data_type()?;
            e = Expr::Cast { expr: Box::new(e), ty };
        }
        Ok(e)
    }

    pub(crate) fn primary(&mut self) -> SqlResult<Expr> {
        let t = self.peek().clone();
        match t.tok {
            Tok::Int(i) => {
                self.advance();
                Ok(Expr::Literal(Value::Int(i)))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(Expr::Literal(Value::Float(f)))
            }
            Tok::Str(s) => {
                self.advance();
                Ok(Expr::Literal(Value::String(s)))
            }
            Tok::Param => {
                self.advance();
                let i = self.next_param;
                self.next_param += 1;
                Ok(Expr::Param(i))
            }
            Tok::NumParam(n) => {
                self.advance();
                Ok(Expr::Param(n - 1))
            }
            Tok::Named(n) => {
                self.advance();
                Ok(Expr::Named(n))
            }
            Tok::Op("(") => {
                self.advance();
                if self.is_kw("SELECT") || self.is_kw("WITH") {
                    let q = self.select()?;
                    self.expect_op(")")?;
                    return Ok(Expr::Subquery(Box::new(q)));
                }
                let e = self.expr()?;
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::QIdent(_) => self.column_ref(),
            Tok::Ident(ref s) => {
                let up = s.to_ascii_uppercase();
                match up.as_str() {
                    "NULL" => {
                        self.advance();
                        Ok(Expr::Literal(Value::Null))
                    }
                    "TRUE" => {
                        self.advance();
                        Ok(Expr::Literal(Value::Bool(true)))
                    }
                    "FALSE" => {
                        self.advance();
                        Ok(Expr::Literal(Value::Bool(false)))
                    }
                    "CASE" => self.case_expr(),
                    "CAST" if matches!(self.peek_at(1), Tok::Op("(")) => {
                        self.advance();
                        self.advance();
                        let e = self.expr()?;
                        self.expect_kw("AS")?;
                        let (ty, _) = self.data_type()?;
                        self.expect_op(")")?;
                        Ok(Expr::Cast { expr: Box::new(e), ty })
                    }
                    "EXISTS" => {
                        self.advance();
                        self.expect_op("(")?;
                        let q = self.select()?;
                        self.expect_op(")")?;
                        Ok(Expr::Exists(Box::new(q)))
                    }
                    "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "SYSDATE" | "SYSTIMESTAMP"
                        if !matches!(self.peek_at(1), Tok::Op("(")) =>
                    {
                        self.advance();
                        let name = if up == "CURRENT_DATE" { "CURRENT_DATE" } else { "NOW" };
                        Ok(Expr::Function { name: name.into(), args: vec![], distinct: false, star: false })
                    }
                    "TIMESTAMP" | "DATE" if matches!(self.peek_at(1), Tok::Str(_)) => {
                        self.advance();
                        let ty = if up == "DATE" { DataType::Date } else { DataType::Timestamp };
                        let lit = self.primary()?;
                        Ok(Expr::Cast { expr: Box::new(lit), ty })
                    }
                    _ if RESERVED.contains(&up.as_str()) => Err(self.unexpected("an expression")),
                    _ => {
                        if matches!(self.peek_at(1), Tok::Op("(")) {
                            self.function_call()
                        } else {
                            self.column_ref()
                        }
                    }
                }
            }
            _ => Err(self.unexpected("an expression")),
        }
    }

    pub(crate) fn column_ref(&mut self) -> SqlResult<Expr> {
        let first = self.ident("column name")?;
        if self.is_op(".") && !matches!(self.peek_at(1), Tok::Op("*")) {
            self.advance();
            let second = self.ident("column name")?;
            if self.is_op(".") {
                // schema.table.column
                self.advance();
                let third = self.ident("column name")?;
                return Ok(Expr::Column { table: Some(second), name: third });
            }
            return Ok(Expr::Column { table: Some(first), name: second });
        }
        Ok(Expr::Column { table: None, name: first })
    }

    pub(crate) fn function_call(&mut self) -> SqlResult<Expr> {
        let name = self.ident("function name")?.to_ascii_uppercase();
        self.expect_op("(")?;
        let mut args = Vec::new();
        let mut distinct = false;
        let mut star = false;
        if self.eat_op("*") {
            star = true;
        } else if !self.is_op(")") {
            if self.eat_kw("DISTINCT") {
                distinct = true;
            } else {
                self.eat_kw("ALL");
            }
            args.push(if name == "POSITION" { self.concat_expr()? } else { self.expr()? });
            loop {
                if self.eat_op(",") || (name == "SUBSTRING" && self.eat_kw("FROM")) {
                    args.push(self.expr()?);
                } else if name == "SUBSTRING" && self.eat_kw("FOR") {
                    if args.len() == 1 {
                        args.push(Expr::Literal(Value::Int(1)));
                    }
                    args.push(self.expr()?);
                } else if name == "POSITION" && self.eat_kw("IN") {
                    args.push(self.expr()?);
                } else {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        if star && name != "COUNT" {
            return Err(SqlErr::syntax(format!("{}(*) is only valid for COUNT", name)));
        }
        let f = Expr::Function { name, args, distinct, star };
        if self.eat_kw("OVER") {
            self.expect_op("(")?;
            let mut partition = Vec::new();
            let mut order = Vec::new();
            if self.eat_kw("PARTITION") {
                self.expect_kw("BY")?;
                partition.push(self.expr()?);
                while self.eat_op(",") {
                    partition.push(self.expr()?);
                }
            }
            if self.eat_kw("ORDER") {
                self.expect_kw("BY")?;
                loop {
                    let expr = self.expr()?;
                    let desc = if self.eat_kw("DESC") {
                        true
                    } else {
                        self.eat_kw("ASC");
                        false
                    };
                    let mut nulls_first = None;
                    if self.eat_kw("NULLS") {
                        nulls_first = Some(self.eat_kw("FIRST"));
                        if nulls_first == Some(false) {
                            self.expect_kw("LAST")?;
                        }
                    }
                    order.push(OrderItem { expr, desc, nulls_first });
                    if !self.eat_op(",") {
                        break;
                    }
                }
            }
            // frame clauses (ROWS / RANGE ...) are accepted; the default frame applies
            let mut depth = 0;
            while !self.at_eof() && !(depth == 0 && self.is_op(")")) {
                if self.is_op("(") {
                    depth += 1;
                } else if self.is_op(")") {
                    depth -= 1;
                }
                self.advance();
            }
            self.expect_op(")")?;
            return Ok(Expr::Window { func: Box::new(f), partition, order });
        }
        Ok(f)
    }

    pub(crate) fn case_expr(&mut self) -> SqlResult<Expr> {
        self.expect_kw("CASE")?;
        let operand = if self.is_kw("WHEN") { None } else { Some(Box::new(self.expr()?)) };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let c = self.expr()?;
            self.expect_kw("THEN")?;
            let v = self.expr()?;
            whens.push((c, v));
        }
        if whens.is_empty() {
            return Err(self.unexpected("WHEN"));
        }
        let else_ = if self.eat_kw("ELSE") { Some(Box::new(self.expr()?)) } else { None };
        self.expect_kw("END")?;
        Ok(Expr::Case { operand, whens, else_ })
    }
}

fn table_ref_name(base: &TableRef, joins: &[Join]) -> String {
    let r = joins.last().map(|j| &j.table).unwrap_or(base);
    match r {
        TableRef::Table { name, alias } => alias.clone().unwrap_or(name.clone()),
        TableRef::Subquery { alias, .. } => alias.clone(),
        TableRef::Function { name, alias, .. } | TableRef::Values { name, alias, .. } => alias.clone().unwrap_or(name.to_ascii_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(crate) fn parses_statements() {
        let v = parse_statements("create table t (id serial primary key, name varchar(20) not null default 'x'); insert into t(name) values ('a'),('b') returning id; select * from t").unwrap();
        assert_eq!(v.len(), 3);
        assert!(matches!(&v[0], Statement::CreateTable { columns, .. } if columns.len() == 2 && columns[0].auto_increment));
        assert!(matches!(&v[1], Statement::Insert { returning: Some(_), .. }));
    }

    #[test]
    pub(crate) fn error_messages() {
        let e = parse_statement("select * form t").unwrap_err();
        assert!(e.message.contains("syntax error at or near \"form\""), "{}", e.message);
        let e = parse_statement("select * from").unwrap_err();
        assert!(e.message.contains("end of input"), "{}", e.message);
        let e = parse_statement("selec 1").unwrap_err();
        assert!(e.message.contains("\"selec\""), "{}", e.message);
        assert_eq!(e.code, "42601");
        let e = parse_statement("create table t (a foo)").unwrap_err();
        assert!(e.message.contains("type \"foo\" does not exist"), "{}", e.message);
    }

    #[test]
    pub(crate) fn counts_params() {
        assert_eq!(count_parameters("select ? , ? from t where a = ?").unwrap(), 3);
        assert_eq!(count_parameters("select $1, $2, $1").unwrap(), 2);
    }
}
