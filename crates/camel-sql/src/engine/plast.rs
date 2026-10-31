//! AST of stored procedures / functions and the procedural language (a PL/pgSQL subset),
//! plus expression walkers used to bind variables.

use super::ast::*;

/// Parameter modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamMode {
    In,
    Out,
    InOut,
}

/// A routine parameter.
#[derive(Clone, Debug)]
pub struct RoutineParam {
    /// Lower-case name (`None` for unnamed parameters, referenced as `$n`).
    pub name: Option<String>,
    pub mode: ParamMode,
    pub ty: DataType,
    pub default: Option<Expr>,
}

/// What a function returns.
#[derive(Clone, Debug)]
pub enum Returns {
    /// Procedures and `RETURNS void`.
    Void,
    Scalar(DataType),
    /// `RETURNS SETOF type` (a scalar type) or `RETURNS SETOF table` (the table's columns).
    SetOf(Option<DataType>, Option<String>),
    /// `RETURNS TABLE (col type, ...)`.
    Table(Vec<(String, DataType)>),
    /// `RETURNS record` (the OUT parameters).
    Record,
}

/// A routine body.
#[derive(Clone, Debug)]
pub enum RoutineBody {
    /// Plain SQL statements (`LANGUAGE sql`): the last statement gives the result.
    Sql(Vec<Statement>),
    /// A procedural block (`[DECLARE ...] BEGIN ... END`).
    Pl(Block),
}

/// A stored procedure or function.
#[derive(Clone, Debug)]
pub struct Routine {
    /// Lower-case name.
    pub name: String,
    pub function: bool,
    pub params: Vec<RoutineParam>,
    pub returns: Returns,
    pub body: RoutineBody,
    /// `STRICT` / `RETURNS NULL ON NULL INPUT`.
    pub strict: bool,
}

/// A procedural variable declaration.
#[derive(Clone, Debug)]
pub struct VarDecl {
    pub name: String,
    /// `None` for `RECORD` / `%TYPE` / `%ROWTYPE` (untyped).
    pub ty: Option<DataType>,
    pub default: Option<Expr>,
    pub constant: bool,
    pub not_null: bool,
}

/// A block: declarations, statements and exception handlers.
#[derive(Clone, Debug, Default)]
pub struct Block {
    pub label: Option<String>,
    pub decls: Vec<VarDecl>,
    pub body: Vec<PlStmt>,
    /// `EXCEPTION WHEN cond [OR cond] THEN ...`: conditions (lower-case names or SQLSTATEs).
    pub handlers: Vec<(Vec<String>, Vec<PlStmt>)>,
}

/// The targets of `SELECT ... INTO` / `RETURNING ... INTO`.
#[derive(Clone, Debug)]
pub struct IntoTargets {
    pub vars: Vec<String>,
    pub strict: bool,
}

/// Procedural statements.
#[derive(Clone, Debug)]
pub enum PlStmt {
    Assign { name: String, field: Option<String>, expr: Expr },
    If { branches: Vec<(Expr, Vec<PlStmt>)>, else_: Vec<PlStmt> },
    While { label: Option<String>, cond: Expr, body: Vec<PlStmt> },
    Loop { label: Option<String>, body: Vec<PlStmt> },
    ForRange { label: Option<String>, var: String, reverse: bool, from: Expr, to: Expr, by: Option<Expr>, body: Vec<PlStmt> },
    ForQuery { label: Option<String>, var: String, query: Box<Statement>, body: Vec<PlStmt> },
    Exit { label: Option<String>, when: Option<Expr> },
    Continue { label: Option<String>, when: Option<Expr> },
    Return(Option<Expr>),
    ReturnNext(Option<Expr>),
    ReturnQuery(Box<Statement>),
    /// `RAISE level 'format' [, args] [USING ERRCODE = '...']`; `None` level re-raises.
    Raise { level: Option<String>, format: String, args: Vec<Expr>, errcode: Option<String> },
    /// An SQL statement, optionally with `INTO` targets.
    Sql { stmt: Box<Statement>, into: Option<IntoTargets> },
    /// `PERFORM expr...` (a SELECT whose result is discarded).
    Perform(Box<Statement>),
    /// `GET DIAGNOSTICS var = ROW_COUNT`.
    GetDiagnostics(Vec<(String, String)>),
    Block(Box<Block>),
    Null,
}

// ---------------------------------------------------------------------------
// walkers
// ---------------------------------------------------------------------------

impl Expr {
    /// Visit this expression and every nested expression (including those of nested
    /// subqueries) in post-order.
    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Expr)) {
        match self {
            Expr::Neg(e) | Expr::Not(e) | Expr::IsNull { expr: e, .. } | Expr::IsBool { expr: e, .. } | Expr::Cast { expr: e, .. } => e.walk_mut(f),
            Expr::Binary { left, right, .. } => {
                left.walk_mut(f);
                right.walk_mut(f);
            }
            Expr::Like { expr, pattern, .. } => {
                expr.walk_mut(f);
                pattern.walk_mut(f);
            }
            Expr::InList { expr, list, .. } => {
                expr.walk_mut(f);
                for e in list {
                    e.walk_mut(f);
                }
            }
            Expr::InSubquery { expr, query, .. } => {
                expr.walk_mut(f);
                query.walk_mut(f);
            }
            Expr::Between { expr, low, high, .. } => {
                expr.walk_mut(f);
                low.walk_mut(f);
                high.walk_mut(f);
            }
            Expr::Function { args, .. } => {
                for a in args {
                    a.walk_mut(f);
                }
            }
            Expr::Case { operand, whens, else_ } => {
                if let Some(o) = operand {
                    o.walk_mut(f);
                }
                for (a, b) in whens {
                    a.walk_mut(f);
                    b.walk_mut(f);
                }
                if let Some(e) = else_ {
                    e.walk_mut(f);
                }
            }
            Expr::Exists(q) | Expr::Subquery(q) => q.walk_mut(f),
            Expr::Literal(_) | Expr::Column { .. } | Expr::Param(_) | Expr::Named(_) => {}
        }
        f(self);
    }

    /// Whether the expression is constant over literals and parameters (no columns,
    /// aggregates or subqueries).
    pub fn is_constant(&self) -> bool {
        let mut c = self.clone();
        let mut constant = true;
        c.walk_mut(&mut |e| match e {
            Expr::Column { .. } | Expr::Exists(_) | Expr::Subquery(_) | Expr::InSubquery { .. } => constant = false,
            Expr::Function { name, .. } if is_aggregate(name) => constant = false,
            _ => {}
        });
        constant
    }
}

impl TableRef {
    fn walk_exprs_mut(&mut self, f: &mut dyn FnMut(&mut Expr)) {
        match self {
            TableRef::Subquery { query, .. } => query.walk_mut(f),
            TableRef::Function { args, .. } => {
                for a in args {
                    a.walk_mut(f);
                }
            }
            TableRef::Table { .. } | TableRef::Values { .. } => {}
        }
    }
}

impl Select {
    /// Visit every expression of the query (see [`Expr::walk_mut`]).
    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Expr)) {
        for it in &mut self.items {
            if let SelectItem::Expr { expr, .. } = it {
                expr.walk_mut(f);
            }
        }
        if let Some(from) = &mut self.from {
            from.base.walk_exprs_mut(f);
            for j in &mut from.joins {
                j.table.walk_exprs_mut(f);
                if let Some(on) = &mut j.on {
                    on.walk_mut(f);
                }
            }
        }
        for e in self.where_.iter_mut().chain(self.having.iter_mut()).chain(self.limit.iter_mut()).chain(self.offset.iter_mut()) {
            e.walk_mut(f);
        }
        for e in &mut self.group_by {
            e.walk_mut(f);
        }
        for o in &mut self.order_by {
            o.expr.walk_mut(f);
        }
        for (_, u) in &mut self.unions {
            u.walk_mut(f);
        }
    }

    /// Visit every table reference of the query, including those of subqueries.
    pub fn walk_tables_mut(&mut self, f: &mut dyn FnMut(&mut TableRef)) {
        if let Some(from) = &mut self.from {
            let mut refs: Vec<&mut TableRef> = vec![&mut from.base];
            refs.extend(from.joins.iter_mut().map(|j| &mut j.table));
            for t in refs {
                if let TableRef::Subquery { query, .. } = t {
                    query.walk_tables_mut(f);
                }
                f(t);
            }
        }
        for (_, u) in &mut self.unions {
            u.walk_tables_mut(f);
        }
        self.walk_mut(&mut |e| {
            if let Expr::Exists(q) | Expr::Subquery(q) | Expr::InSubquery { query: q, .. } = e {
                q.walk_tables_mut(f);
            }
        });
    }
}

impl Statement {
    /// Visit every expression of the statement (see [`Expr::walk_mut`]).
    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Expr)) {
        fn items(items: &mut [SelectItem], f: &mut dyn FnMut(&mut Expr)) {
            for it in items {
                if let SelectItem::Expr { expr, .. } = it {
                    expr.walk_mut(f);
                }
            }
        }
        match self {
            Statement::Select(s) => s.walk_mut(f),
            Statement::Insert { source, returning, .. } => {
                match source {
                    InsertSource::Values(rows) => {
                        for r in rows {
                            for e in r {
                                e.walk_mut(f);
                            }
                        }
                    }
                    InsertSource::Select(s) => s.walk_mut(f),
                    InsertSource::DefaultValues => {}
                }
                if let Some(r) = returning {
                    items(r, f);
                }
            }
            Statement::Update { sets, where_, returning, .. } => {
                for (_, e) in sets {
                    e.walk_mut(f);
                }
                if let Some(w) = where_ {
                    w.walk_mut(f);
                }
                if let Some(r) = returning {
                    items(r, f);
                }
            }
            Statement::Delete { where_, returning, .. } => {
                if let Some(w) = where_ {
                    w.walk_mut(f);
                }
                if let Some(r) = returning {
                    items(r, f);
                }
            }
            Statement::Call { args, .. } => {
                for a in args {
                    a.walk_mut(f);
                }
            }
            _ => {}
        }
    }

    /// Visit every table reference of the statement (FROM clauses and subqueries).
    pub fn walk_tables_mut(&mut self, f: &mut dyn FnMut(&mut TableRef)) {
        if let Statement::Select(s) = self {
            s.walk_tables_mut(f);
            return;
        }
        if let Statement::Insert { source: InsertSource::Select(s), .. } = self {
            s.walk_tables_mut(f);
        }
        self.walk_mut(&mut |e| {
            if let Expr::Exists(q) | Expr::Subquery(q) | Expr::InSubquery { query: q, .. } = e {
                q.walk_tables_mut(f);
            }
        });
    }

    /// Names (lower-case) of the tables the statement reads or writes.
    pub fn table_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self {
            Statement::Insert { table, .. } | Statement::Update { table, .. } | Statement::Delete { table, .. } => out.push(table.to_ascii_lowercase()),
            _ => {}
        }
        let mut c = self.clone();
        c.walk_tables_mut(&mut |t| {
            if let TableRef::Table { name, .. } = t {
                out.push(name.to_ascii_lowercase());
            }
        });
        out.sort();
        out.dedup();
        out
    }
}
