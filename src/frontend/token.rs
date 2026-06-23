//! Token definitions for the Ran language.

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub lexeme: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    IntLiteral,
    FloatLiteral,
    StringLiteral,
    Variable, // $name or ${name}
    Identifier,

    // Keywords
    Fn,
    Let,
    Mut,
    /// `var` — Go-style mutable declaration (sugar for `let mut`).
    Var,
    If,
    Else,
    For,
    While,
    Loop,
    Break,
    Continue,
    Return,
    Spawn, // goroutine-style concurrency
    Chan,  // channel
    Struct,
    Enum,
    Impl,
    Trait,
    Pub,
    Use,
    Mod,
    True,
    False,
    Echo, // bash echo
    Import,
    Export,
    Async,
    Await,
    Match,
    Type,
    Const,
    Unsafe,
    In,

    // Operators
    Assign,       // =
    Plus,         // +
    Minus,        // -
    Star,         // *
    Slash,        // /
    Percent,      // %
    Bang,         // !
    EqualEqual,   // ==
    BangEqual,    // !=
    Less,         // <
    LessEqual,    // <=
    Greater,      // >
    GreaterEqual, // >=
    AmpAmp,       // &&
    PipePipe,     // ||
    Amp,          // & (reference/borrow)
    Pipe,         // | (pipe operator, like bash)
    Arrow,        // <- (channel send/receive)
    RightArrow,   // -> (return type)
    FatArrow,     // => (match arm)

    // Delimiters
    LeftParen,    // (
    RightParen,   // )
    LeftBrace,    // {
    RightBrace,   // }
    LeftBracket,  // [
    RightBracket, // ]
    Comma,        // ,
    Dot,          // .
    Colon,        // :
    Semicolon,    // ;

    // Special
    Newline,
    Eof,
}

impl std::fmt::Display for TokenKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
