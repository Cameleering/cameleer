//! The abstract syntax tree shared by every language of this crate.
//!
//! All parsers (JavaScript, Groovy, Python, OGNL, MVEL, SpEL, DataSonnet) produce this tree and a
//! single interpreter ([`crate::interp`]) evaluates it, with a [`Dialect`] selecting the
//! language-specific semantics (truthiness, equality, division, string conversion, ...).
//! The tree is immutable and `Send + Sync`, so a parsed script is shared between threads.

use std::sync::Arc;

/// The language whose semantics the interpreter applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Js,
    Groovy,
    Python,
    Ognl,
    Mvel,
    Spel,
    Jsonnet,
    /// Java source snippets (Camel's `joor` / `java` language).
    Java,
}

impl Dialect {
    /// Human readable language name (used in error messages).
    pub fn display_name(self) -> &'static str {
        match self {
            Dialect::Js => "JavaScript",
            Dialect::Groovy => "Groovy",
            Dialect::Python => "Python",
            Dialect::Ognl => "OGNL",
            Dialect::Mvel => "MVEL",
            Dialect::Spel => "SpEL",
            Dialect::Jsonnet => "DataSonnet",
            Dialect::Java => "Java",
        }
    }
    /// Java-like expression languages (integer division, Java `toString`).
    pub fn is_java(self) -> bool {
        matches!(self, Dialect::Ognl | Dialect::Mvel | Dialect::Spel | Dialect::Java)
    }
    /// Groovy or Java: Java/GDK collection and string methods, typed declarations.
    pub fn groovy_like(self) -> bool {
        matches!(self, Dialect::Groovy | Dialect::Java)
    }
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    FloorDiv,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    Cmp,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    NotIn,
    InstanceOf,
    NotInstanceOf,
    /// Groovy `=~` (find).
    Find,
    /// Groovy `==~` / SpEL `matches` (full match).
    Matches,
    /// MVEL `contains`.
    Contains,
    /// Python `is`.
    Is,
    /// Python `is not`.
    IsNot,
    /// Groovy `<<` on collections (append) is handled by `Shl`.
    Between,
}

/// Unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
    Plus,
    BitNot,
    Typeof,
    Void,
    Delete,
}

/// A template literal / GString / f-string part.
#[derive(Clone, Debug)]
pub enum TplPart {
    Lit(String),
    /// An interpolated expression with an optional Python format spec (`{x:.2f}`) and conversion (`!r`).
    Expr(Expr, Option<String>, Option<char>),
}

/// An element of an array literal or a call argument list.
#[derive(Clone, Debug)]
pub enum Elem {
    Item(Expr),
    Spread(Expr),
    Hole,
}

/// A property key in an object literal.
#[derive(Clone, Debug)]
pub enum PropKey {
    Name(String),
    Computed(Expr),
}

/// A property of an object / map literal.
#[derive(Clone, Debug)]
pub enum Prop {
    KV(PropKey, Expr),
    Spread(Expr),
    /// `get name() {...}`
    Get(PropKey, Arc<FuncDef>),
    /// `set name(v) {...}`
    Set(PropKey, Arc<FuncDef>),
}

/// Selection mode of OGNL / SpEL collection selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    All,
    First,
    Last,
}

/// Comprehension clause (Python, Jsonnet).
#[derive(Clone, Debug)]
pub enum CompClause {
    For(Pat, Expr),
    If(Expr),
}

/// Kind of comprehension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompKind {
    List,
    Dict,
    Set,
}

/// Expressions.
#[derive(Clone, Debug)]
pub enum Expr {
    Undef,
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Template(Vec<TplPart>),
    Regex(String, String),
    Ident(String, u32),
    This,
    Super(u32),
    Array(Vec<Elem>),
    Object(Vec<Prop>),
    Func(Arc<FuncDef>),
    Class(Arc<ClassDef>),
    Member { obj: Box<Expr>, name: String, optional: bool, pos: u32 },
    Index { obj: Box<Expr>, index: Box<Expr>, optional: bool, pos: u32 },
    Slice { obj: Box<Expr>, start: Option<Box<Expr>>, end: Option<Box<Expr>>, step: Option<Box<Expr>>, pos: u32 },
    Call { callee: Box<Expr>, args: Vec<Elem>, optional: bool, pos: u32 },
    New { callee: Box<Expr>, args: Vec<Elem>, pos: u32 },
    Unary(UnOp, Box<Expr>, u32),
    Update { inc: bool, prefix: bool, target: Box<Expr>, pos: u32 },
    Bin(BinOp, Box<Expr>, Box<Expr>, u32),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Nullish(Box<Expr>, Box<Expr>),
    Elvis(Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Assign { op: Option<AssignOp>, target: Box<Pat>, value: Box<Expr>, pos: u32 },
    Seq(Vec<Expr>),
    Range { from: Box<Expr>, to: Box<Expr>, exclusive: bool },
    As(Box<Expr>, String, u32),
    /// Groovy spread-dot `list*.name`.
    SpreadMember { obj: Box<Expr>, name: String },
    /// OGNL `list.{expr}` / SpEL `list.![expr]`.
    Project { obj: Box<Expr>, expr: Box<Expr> },
    /// OGNL `list.{? expr}` / SpEL `list.?[expr]`.
    Select { obj: Box<Expr>, expr: Box<Expr>, mode: SelectMode },
    Comp { kind: CompKind, key: Box<Expr>, value: Option<Box<Expr>>, clauses: Vec<CompClause> },
    /// Jsonnet `local a = 1, b = 2; body`.
    Let(Vec<(String, Expr)>, Box<Expr>),
    /// Jsonnet `error expr`.
    Error(Box<Expr>, u32),
    /// A type literal (`String`, `java.lang.Integer`, SpEL `T(Math)`).
    Type(String),
    /// `yield expr` / `yield* expr` (generators).
    Yield { arg: Option<Box<Expr>>, delegate: bool, pos: u32 },
    /// `await expr` (async functions).
    Await(Box<Expr>, u32),
    /// DataSonnet object literal with `self` / `super` / hidden fields.
    JObject(Arc<JObjDef>),
    /// DataSonnet `self` / `super` / `$`.
    JSelf(u8, u32),
    /// DataSonnet `import` / `importstr` / `importbin` of a local file.
    Import(u8, String, u32),
    /// Java switch expression: (labels (empty = default), arm body).
    SwitchExpr(Box<Expr>, Vec<(Vec<Expr>, Vec<Stmt>)>, u32),
    /// Java `x instanceof Type name` (binds `name` when it matches).
    InstanceBind { expr: Box<Expr>, ty: String, name: String, pos: u32 },
    /// Java method reference `Type::method` / `obj::method`.
    MethodRef(Box<Expr>, String, u32),
}

/// Visibility of a DataSonnet field (`:`, `::`, `:::`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Default,
    Hidden,
    Visible,
}

/// A DataSonnet object field definition.
#[derive(Clone, Debug)]
pub struct JFieldDef {
    pub key: PropKey,
    pub vis: Vis,
    /// `+:` (merge with the inherited value).
    pub plus: bool,
    pub value: Expr,
}

/// A DataSonnet object literal.
#[derive(Clone, Debug)]
pub struct JObjDef {
    pub locals: Vec<(String, Expr)>,
    pub fields: Vec<JFieldDef>,
    pub asserts: Vec<(Expr, Expr, u32)>,
}

/// Compound assignment operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Bin(BinOp),
    And,
    Or,
    Nullish,
    Elvis,
}

/// Binding / assignment patterns.
#[derive(Clone, Debug)]
pub enum Pat {
    Id(String),
    /// `[a, , b = 1, ...rest]` / Python `a, b`.
    Arr(Vec<Option<(Pat, Option<Expr>)>>, Option<Box<Pat>>),
    /// `{a, b: c = 1, ...rest}`.
    Obj(Vec<(PropKey, Pat, Option<Expr>)>, Option<Box<Pat>>),
    /// An assignment target expression (member / index / identifier).
    Target(Expr),
}

/// A function parameter.
#[derive(Clone, Debug)]
pub struct Param {
    pub pat: Pat,
    pub default: Option<Expr>,
    /// Declared type (Groovy / Java), used for overload resolution.
    pub ty: Option<String>,
}

/// Function body.
#[derive(Clone, Debug)]
pub enum FuncBody {
    Block(Vec<Stmt>),
    Expr(Box<Expr>),
}

/// A function / closure / lambda / method definition.
#[derive(Clone, Debug)]
pub struct FuncDef {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub rest: Option<Pat>,
    pub body: FuncBody,
    /// Arrow function: lexical `this`.
    pub arrow: bool,
    /// Groovy closure without declared parameters: binds `it`.
    pub implicit_it: bool,
    /// The value of the last expression statement is returned (Groovy).
    pub implicit_return: bool,
    /// `function*` / Python function containing `yield`.
    pub generator: bool,
    /// `async function`.
    pub is_async: bool,
}

impl FuncDef {
    /// A plain function definition.
    pub fn new(name: Option<String>, params: Vec<Param>, rest: Option<Pat>, body: FuncBody) -> FuncDef {
        FuncDef { name, params, rest, body, arrow: false, implicit_it: false, implicit_return: false, generator: false, is_async: false }
    }
}

/// Kind of a class member function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Getter,
    Setter,
    /// Python `@classmethod`.
    ClassMethod,
}

/// A class member function.
#[derive(Clone, Debug)]
pub struct ClassMember {
    pub name: String,
    /// Computed member name (`[Symbol.iterator]() {...}`).
    pub key: Option<Expr>,
    pub func: Arc<FuncDef>,
    pub kind: MemberKind,
    pub is_static: bool,
}

/// A class field (JavaScript class field, Groovy property, Python class attribute).
#[derive(Clone, Debug)]
pub struct ClassField {
    pub name: String,
    pub init: Option<Expr>,
    pub is_static: bool,
}

/// Language flavor of a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassStyle {
    Js,
    Groovy,
    Python,
}

/// A class (JavaScript, Groovy, Python).
#[derive(Clone, Debug)]
pub struct ClassDef {
    pub name: Option<String>,
    pub extends: Option<Expr>,
    /// Constructors (JS: at most one; Groovy: overloads chosen by arity; Python: `__init__`).
    pub ctors: Vec<Arc<FuncDef>>,
    pub methods: Vec<ClassMember>,
    pub fields: Vec<ClassField>,
    /// Groovy traits / interfaces with default methods mixed in (`implements`).
    pub mixins: Vec<Expr>,
    pub style: ClassStyle,
    /// Groovy AST transform annotations (`ToString`, `EqualsAndHashCode`, `TupleConstructor`, `Canonical`...).
    pub annotations: Vec<String>,
}

/// Declaration kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

/// A `catch` / `except` clause.
#[derive(Clone, Debug)]
pub struct Catch {
    /// Exception type names (empty = catch everything).
    pub types: Vec<String>,
    pub param: Option<Pat>,
    pub body: Vec<Stmt>,
}

/// Statements.
#[derive(Clone, Debug)]
pub enum Stmt {
    Expr(Expr),
    Var(VarKind, Vec<(Pat, Option<Expr>)>),
    Func(Arc<FuncDef>),
    Class(Arc<ClassDef>),
    Return(Option<Expr>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Vec<Stmt>),
    For { init: Option<Box<Stmt>>, test: Option<Expr>, update: Option<Expr>, body: Box<Stmt> },
    /// `for (x in obj)` (keys) when `values` is false; for-of / Groovy for-in / Python for when true.
    ForIn { decl: Option<VarKind>, pat: Pat, obj: Expr, body: Box<Stmt>, values: bool, else_body: Option<Box<Stmt>> },
    While(Expr, Box<Stmt>, Option<Box<Stmt>>),
    DoWhile(Box<Stmt>, Expr),
    Break(Option<String>),
    Continue(Option<String>),
    Throw(Expr, u32),
    Try { block: Vec<Stmt>, catches: Vec<Catch>, finally: Option<Vec<Stmt>> },
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(String, Box<Stmt>),
    Empty,
    Global(Vec<String>),
    Assert(Expr, Option<Expr>, u32),
    /// Java `yield value;` inside a switch expression arm.
    SwitchYield(Expr),
}

/// A parsed program.
#[derive(Clone, Debug)]
pub struct Program {
    pub dialect: Dialect,
    pub body: Vec<Stmt>,
}
