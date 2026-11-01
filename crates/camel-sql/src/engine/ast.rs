//! SQL abstract syntax tree.

use camel_core::value::Value;

pub use super::plast::*;

/// Column data types.
#[derive(Clone, Debug, PartialEq)]
pub enum DataType {
    Integer,
    BigInt,
    /// VARCHAR / CHAR with optional maximum length.
    Varchar(Option<usize>),
    Text,
    Boolean,
    Double,
    /// DECIMAL / NUMERIC (precision, scale); values are stored as floats.
    Decimal(Option<u32>, Option<u32>),
    Timestamp,
    Date,
    Bytes,
}

impl DataType {
    /// SQL spelling used in messages.
    pub fn sql_name(&self) -> String {
        match self {
            DataType::Integer => "integer".into(),
            DataType::BigInt => "bigint".into(),
            DataType::Varchar(Some(n)) => format!("character varying({})", n),
            DataType::Varchar(None) => "character varying".into(),
            DataType::Text => "text".into(),
            DataType::Boolean => "boolean".into(),
            DataType::Double => "double precision".into(),
            DataType::Decimal(..) => "numeric".into(),
            DataType::Timestamp => "timestamp".into(),
            DataType::Date => "date".into(),
            DataType::Bytes => "bytea".into(),
        }
    }
}

/// A column definition in CREATE TABLE.
#[derive(Clone, Debug)]
pub struct ColumnDef {
    pub name: String,
    pub ty: DataType,
    pub not_null: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub default: Option<Expr>,
    pub auto_increment: bool,
}

/// Table-level constraints.
#[derive(Clone, Debug)]
pub enum TableConstraint {
    PrimaryKey(Vec<String>),
    Unique(Vec<String>),
}

/// A parsed statement.
#[derive(Clone, Debug)]
pub enum Statement {
    CreateTable { name: String, if_not_exists: bool, columns: Vec<ColumnDef>, constraints: Vec<TableConstraint> },
    DropTable { names: Vec<String>, if_exists: bool },
    Truncate { name: String },
    /// CREATE INDEX / ... statements that are accepted and ignored.
    NoOp { command: String },
    /// `ALTER TABLE name action`.
    AlterTable { name: String, if_exists: bool, action: AlterAction },
    Insert { table: String, columns: Option<Vec<String>>, source: InsertSource, returning: Option<Vec<SelectItem>> },
    Select(Box<Select>),
    Update { table: String, alias: Option<String>, sets: Vec<(String, Expr)>, where_: Option<Expr>, returning: Option<Vec<SelectItem>> },
    Delete { table: String, alias: Option<String>, where_: Option<Expr>, returning: Option<Vec<SelectItem>> },
    /// `CREATE [OR REPLACE] PROCEDURE | FUNCTION`.
    CreateRoutine { routine: Box<Routine>, or_replace: bool },
    /// `DROP PROCEDURE | FUNCTION [IF EXISTS] name`.
    DropRoutine { name: String, function: bool, if_exists: bool },
    /// `CALL name(args)`.
    Call { name: String, args: Vec<Expr> },
    /// `DO $$ block $$` (anonymous procedural block).
    Do(Box<Block>),
    Begin,
    Commit,
    Rollback,
}

/// An `ALTER TABLE` action.
#[derive(Clone, Debug)]
pub enum AlterAction {
    AddColumn { def: ColumnDef, if_not_exists: bool },
    DropColumn { name: String, if_exists: bool },
    RenameColumn { from: String, to: String },
    RenameTable { to: String },
    SetDefault { column: String, default: Option<Expr> },
    SetNotNull { column: String, not_null: bool },
    SetType { column: String, ty: DataType },
    AddConstraint { name: Option<String>, constraint: TableConstraint },
    DropConstraint { name: String, if_exists: bool },
}

/// A common table expression (`WITH name [(cols)] AS (query)`).
#[derive(Clone, Debug)]
pub struct Cte {
    pub name: String,
    pub columns: Option<Vec<String>>,
    pub query: Box<Select>,
    pub recursive: bool,
}

/// Rows of an INSERT.
#[derive(Clone, Debug)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Select(Box<Select>),
    DefaultValues,
}

/// SELECT query (optionally combined with UNION [ALL]).
#[derive(Clone, Debug, Default)]
pub struct Select {
    pub distinct: bool,
    pub items: Vec<SelectItem>,
    pub from: Option<From>,
    pub where_: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderItem>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    /// `UNION [ALL] select` parts (applied before ORDER BY / LIMIT).
    pub unions: Vec<(bool, Select)>,
    /// `WITH [RECURSIVE]` common table expressions.
    pub ctes: Vec<Cte>,
}

/// An ORDER BY item.
#[derive(Clone, Debug)]
pub struct OrderItem {
    pub expr: Expr,
    pub desc: bool,
    /// Explicit NULLS FIRST (true) / NULLS LAST (false).
    pub nulls_first: Option<bool>,
}

/// A select list item.
#[derive(Clone, Debug)]
pub enum SelectItem {
    Wildcard,
    QualifiedWildcard(String),
    Expr { expr: Expr, alias: Option<String> },
}

/// FROM clause.
#[derive(Clone, Debug)]
pub struct From {
    pub base: TableRef,
    pub joins: Vec<Join>,
}

/// A table or subquery in FROM.
#[derive(Clone, Debug)]
pub enum TableRef {
    Table { name: String, alias: Option<String> },
    Subquery { query: Box<Select>, alias: String },
    /// A set-returning function call (`FROM fn(args) [alias]`).
    Function { name: String, args: Vec<Expr>, alias: Option<String> },
    /// A materialized relation (internal: the pre-evaluated result of a function call).
    Values { name: String, columns: Vec<String>, rows: Vec<Vec<Value>>, alias: Option<String> },
}

/// Join kinds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

/// A join.
#[derive(Clone, Debug)]
pub struct Join {
    pub kind: JoinKind,
    pub table: TableRef,
    pub on: Option<Expr>,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
}

/// Expressions.
#[derive(Clone, Debug)]
pub enum Expr {
    Literal(Value),
    Column { table: Option<String>, name: String },
    /// Positional parameter (0-based index).
    Param(usize),
    Named(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
    IsNull { expr: Box<Expr>, negated: bool },
    IsBool { expr: Box<Expr>, value: bool, negated: bool },
    Like { expr: Box<Expr>, pattern: Box<Expr>, negated: bool, case_insensitive: bool, escape: Option<char> },
    InList { expr: Box<Expr>, list: Vec<Expr>, negated: bool },
    InSubquery { expr: Box<Expr>, query: Box<Select>, negated: bool },
    Between { expr: Box<Expr>, low: Box<Expr>, high: Box<Expr>, negated: bool },
    Function { name: String, args: Vec<Expr>, distinct: bool, star: bool },
    Case { operand: Option<Box<Expr>>, whens: Vec<(Expr, Expr)>, else_: Option<Box<Expr>> },
    Cast { expr: Box<Expr>, ty: DataType },
    Exists(Box<Select>),
    Subquery(Box<Select>),
    /// `fn(args) OVER ([PARTITION BY ..] [ORDER BY ..])`.
    Window { func: Box<Expr>, partition: Vec<Expr>, order: Vec<OrderItem> },
}

/// Aggregate function names.
pub fn is_aggregate(name: &str) -> bool {
    matches!(name, "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "STRING_AGG" | "GROUP_CONCAT" | "BOOL_AND" | "BOOL_OR" | "EVERY")
}

impl Expr {
    /// Whether the expression contains an aggregate call (not looking into subqueries).
    pub fn contains_aggregate(&self) -> bool {
        match self {
            Expr::Function { name, args, .. } => is_aggregate(name) || args.iter().any(|a| a.contains_aggregate()),
            Expr::Neg(e) | Expr::Not(e) => e.contains_aggregate(),
            Expr::Binary { left, right, .. } => left.contains_aggregate() || right.contains_aggregate(),
            Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } | Expr::Cast { expr, .. } => expr.contains_aggregate(),
            Expr::Like { expr, pattern, .. } => expr.contains_aggregate() || pattern.contains_aggregate(),
            Expr::InList { expr, list, .. } => expr.contains_aggregate() || list.iter().any(|e| e.contains_aggregate()),
            Expr::InSubquery { expr, .. } => expr.contains_aggregate(),
            Expr::Between { expr, low, high, .. } => expr.contains_aggregate() || low.contains_aggregate() || high.contains_aggregate(),
            Expr::Case { operand, whens, else_ } => {
                operand.as_ref().is_some_and(|o| o.contains_aggregate())
                    || whens.iter().any(|(a, b)| a.contains_aggregate() || b.contains_aggregate())
                    || else_.as_ref().is_some_and(|e| e.contains_aggregate())
            }
            _ => false,
        }
    }
}
