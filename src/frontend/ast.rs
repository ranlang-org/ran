//! Abstract Syntax Tree definitions for the Ran language.
//! Represents the structure of a Ran program after parsing.

/// Source location for a node (1-based line and column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// A statement together with its source location.
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: Statement,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: Statement, span: Span) -> Self {
        Self { kind, span }
    }
}

/// A complete Ran program
#[derive(Debug, Clone)]
pub struct Program {
    pub statements: Vec<Stmt>,
}

/// Top-level statements
#[derive(Debug, Clone)]
pub enum Statement {
    /// Variable assignment: name="value" or let name = value
    VarDecl {
        name: String,
        mutable: bool,
        type_annotation: Option<TypeExpr>,
        value: Expression,
    },

    /// Function declaration: fn name(params) -> RetType { body }
    FnDecl {
        name: String,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Vec<Stmt>,
        is_pub: bool,
        is_async: bool,
    },

    /// Struct declaration
    StructDecl {
        name: String,
        fields: Vec<Field>,
        is_pub: bool,
    },

    /// Enum declaration
    EnumDecl {
        name: String,
        variants: Vec<EnumVariant>,
        is_pub: bool,
    },

    /// Impl block
    ImplBlock {
        type_name: String,
        trait_name: Option<String>,
        methods: Vec<Stmt>,
    },

    /// Trait declaration: a named set of method signatures, each optionally
    /// carrying a default body. A method whose `body` is empty is a pure
    /// signature; a non-empty `body` is a default implementation that an
    /// `impl TraitName for TypeName` block inherits unless it overrides it.
    TraitDecl {
        name: String,
        methods: Vec<Stmt>,
        is_pub: bool,
    },

    /// Expression statement
    Expr(Expression),

    /// echo "hello" - bash-style print. `escapes` is set by `echo -e`.
    Echo { expr: Expression, escapes: bool },

    /// return expression
    Return(Option<Expression>),

    /// break out of the innermost enclosing loop
    Break,

    /// continue to the next iteration of the innermost enclosing loop
    Continue,

    /// if condition { } else { }
    If {
        condition: Expression,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },

    /// for item in iterable { }
    For {
        variable: String,
        iterable: Expression,
        body: Vec<Stmt>,
    },

    /// while condition { }
    While {
        condition: Expression,
        body: Vec<Stmt>,
    },

    /// spawn { } - Go-style concurrency
    Spawn { body: Vec<Stmt> },

    /// use/import module: import "http" as http
    Import { path: String, alias: Option<String> },
}

/// Expressions
#[derive(Debug, Clone)]
pub enum Expression {
    /// Integer literal
    IntLiteral(i64),

    /// Float literal
    FloatLiteral(f64),

    /// String literal (supports interpolation)
    StringLiteral(String),

    /// Boolean
    BoolLiteral(bool),

    /// Variable reference: $name or just name
    Variable(String),

    /// Binary operation: a + b, a == b, etc.
    BinaryOp {
        left: Box<Expression>,
        op: BinaryOperator,
        right: Box<Expression>,
    },

    /// Unary operation: !x, -x, &x, *x
    UnaryOp {
        op: UnaryOperator,
        operand: Box<Expression>,
    },

    /// Function call: foo(args)
    FnCall {
        callee: Box<Expression>,
        args: Vec<Expression>,
    },

    /// Method call: obj.method(args)
    MethodCall {
        object: Box<Expression>,
        method: String,
        args: Vec<Expression>,
    },

    /// Field access: obj.field
    FieldAccess {
        object: Box<Expression>,
        field: String,
    },

    /// Index: arr[i]
    Index {
        object: Box<Expression>,
        index: Box<Expression>,
    },

    /// Pipe expression: cmd1 | cmd2 (bash-style)
    Pipe {
        left: Box<Expression>,
        right: Box<Expression>,
    },

    /// Channel send: chan <- value
    ChanSend {
        channel: Box<Expression>,
        value: Box<Expression>,
    },

    /// Channel receive: <- chan
    ChanRecv { channel: Box<Expression> },

    /// Closure / lambda: fn(params) { body }
    Lambda {
        params: Vec<Param>,
        body: Vec<Stmt>,
    },

    /// Struct instantiation: Name { field: value }
    StructInit {
        name: String,
        fields: Vec<(String, Expression)>,
    },

    /// Array literal: [1, 2, 3]
    Array(Vec<Expression>),

    /// Await expression
    Await(Box<Expression>),

    /// Match expression
    Match {
        subject: Box<Expression>,
        arms: Vec<MatchArm>,
    },
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Literal(Expression),
    Variable(String),
    Wildcard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub enum UnaryOperator {
    Neg,
    Not,
    Ref,    // & (borrow)
    Deref,  // * (dereference)
    MutRef, // &mut (mutable borrow)
}

/// Function parameter
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub type_annotation: Option<TypeExpr>,
    pub is_mut: bool,
}

/// Struct field
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub type_annotation: TypeExpr,
    pub is_pub: bool,
}

/// Enum variant
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Option<Vec<TypeExpr>>,
}

/// Type expressions
#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// Simple type: i32, String, bool
    Named(String),

    /// Reference: &T or &mut T
    Ref { mutable: bool, inner: Box<TypeExpr> },

    /// Array: [T] or Vec<T>
    Array(Box<TypeExpr>),

    /// Generic: Option<T>, Result<T, E>
    Generic { name: String, params: Vec<TypeExpr> },

    /// Function type: fn(A, B) -> C
    Function {
        params: Vec<TypeExpr>,
        return_type: Box<TypeExpr>,
    },

    /// Channel type: chan<T>
    Channel(Box<TypeExpr>),
}
