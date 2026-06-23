//! Parser module - Transforms tokens into an AST.
//! Handles bash-style syntax (var="value"), Rust-style types, and Go-style concurrency.

use crate::frontend::ast::*;
use crate::frontend::lexer::{Token, TokenKind};
use crate::support::diagnostics::{Diagnostic, SourceLoc};

/// Parse a stream of tokens into a Program AST (errors discarded).
/// Prefer `parse_checked` in the compile pipeline so syntax errors abort.
pub fn parse(tokens: Vec<Token>) -> Program {
    parse_checked(tokens).0
}

/// Parse tokens, returning the AST together with any syntax diagnostics.
/// A non-empty diagnostics vector means the program must NOT be executed.
pub fn parse_checked(tokens: Vec<Token>) -> (Program, Vec<Diagnostic>) {
    let mut parser = Parser::new(tokens);
    let program = parser.parse_program();
    (program, parser.errors)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<Diagnostic>,
    /// When true, an `Ident {` is parsed as a block boundary, not a struct
    /// literal. Set while parsing `if`/`while`/`for` headers to avoid ambiguity
    /// (same technique Rust uses for struct-literals in condition position).
    no_struct_literal: bool,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, errors: Vec::new(), no_struct_literal: false }
    }

    /// Record a syntax error at the current token position.
    fn error(&mut self, code: &str, message: impl Into<String>, help: impl Into<String>) {
        let (line, col) = if self.pos < self.tokens.len() {
            (self.tokens[self.pos].span.line, self.tokens[self.pos].span.col)
        } else if let Some(last) = self.tokens.last() {
            (last.span.line, last.span.col)
        } else {
            (1, 1)
        };
        let loc = SourceLoc::new("", line, col, col + 1);
        self.errors.push(
            Diagnostic::error(message.into())
                .with_code(code)
                .with_loc(loc.clone())
                .with_label(loc, "here")
                .with_help(help.into()),
        );
    }

    fn parse_program(&mut self) -> Program {
        let mut statements = Vec::new();

        while !self.is_at_end() {
            self.skip_newlines();
            if self.is_at_end() {
                break;
            }
            if let Some(stmt) = self.parse_stmt() {
                statements.push(stmt);
            }
        }

        Program { statements }
    }

    /// Current token's source location (line, col).
    fn current_span(&self) -> Span {
        if self.pos < self.tokens.len() {
            let s = &self.tokens[self.pos].span;
            Span::new(s.line, s.col)
        } else {
            Span::new(0, 0)
        }
    }

    /// Parse a statement and attach its source span.
    fn parse_stmt(&mut self) -> Option<Stmt> {
        let span = self.current_span();
        self.parse_statement().map(|kind| Stmt::new(kind, span))
    }

    fn parse_statement(&mut self) -> Option<Statement> {
        match self.current_kind() {
            TokenKind::Fn => Some(self.parse_fn_decl(false, false)),
            TokenKind::Pub => {
                self.advance();
                match self.current_kind() {
                    TokenKind::Fn => Some(self.parse_fn_decl(true, false)),
                    TokenKind::Struct => Some(self.parse_struct_decl(true)),
                    TokenKind::Enum => Some(self.parse_enum_decl(true)),
                    TokenKind::Trait => Some(self.parse_trait_decl(true)),
                    _ => None,
                }
            }
            TokenKind::Async => {
                self.advance();
                if self.current_kind() == TokenKind::Fn {
                    Some(self.parse_fn_decl(false, true))
                } else {
                    None
                }
            }
            TokenKind::Let => Some(self.parse_let_decl()),
            TokenKind::Var => Some(self.parse_var_decl()),
            TokenKind::Struct => Some(self.parse_struct_decl(false)),
            TokenKind::Enum => Some(self.parse_enum_decl(false)),
            TokenKind::Impl => Some(self.parse_impl_block()),
            TokenKind::Trait => Some(self.parse_trait_decl(false)),
            TokenKind::If => Some(self.parse_if()),
            TokenKind::For => Some(self.parse_for()),
            TokenKind::While => Some(self.parse_while()),
            TokenKind::Spawn => Some(self.parse_spawn()),
            TokenKind::Echo => Some(self.parse_echo()),
            TokenKind::Return => Some(self.parse_return()),
            TokenKind::Break => {
                self.advance(); // consume 'break'
                Some(Statement::Break)
            }
            TokenKind::Continue => {
                self.advance(); // consume 'continue'
                Some(Statement::Continue)
            }
            TokenKind::Import | TokenKind::Use => Some(self.parse_import()),
            TokenKind::Match => Some(Statement::Expr(self.parse_expression())),
            // Expression statements that begin with a literal/operator/grouping.
            TokenKind::IntLiteral
            | TokenKind::FloatLiteral
            | TokenKind::StringLiteral
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Variable
            | TokenKind::LeftParen
            | TokenKind::LeftBracket
            | TokenKind::Minus
            | TokenKind::Bang
            | TokenKind::Star
            | TokenKind::Amp
            | TokenKind::Arrow => Some(Statement::Expr(self.parse_expression())),
            TokenKind::Identifier => {
                // Check if it's a bash-style assignment: name="value"
                if self.peek_kind() == Some(TokenKind::Assign) {
                    Some(self.parse_bash_assign())
                } else {
                    Some(Statement::Expr(self.parse_expression()))
                }
            }
            TokenKind::Eof => {
                self.advance();
                None
            }
            _ => {
                self.error(
                    "E0102",
                    format!("expected a statement, found `{:?}`", self.current_kind()),
                    "statements start with a keyword (fn, let, if, ...), an assignment, or an expression",
                );
                self.advance();
                None
            }
        }
    }

    fn parse_fn_decl(&mut self, is_pub: bool, is_async: bool) -> Statement {
        self.advance(); // consume 'fn'
        let name = self.consume_identifier();
        self.expect(TokenKind::LeftParen);
        let params = self.parse_params();
        self.expect(TokenKind::RightParen);

        let return_type = if self.current_kind() == TokenKind::RightArrow {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        let body = self.parse_block();

        Statement::FnDecl {
            name,
            params,
            return_type,
            body,
            is_pub,
            is_async,
        }
    }

    fn parse_let_decl(&mut self) -> Statement {
        self.advance(); // consume 'let'

        let mutable = if self.current_kind() == TokenKind::Mut {
            self.advance();
            true
        } else {
            false
        };

        let name = self.consume_identifier();

        let type_annotation = if self.current_kind() == TokenKind::Colon {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.expect(TokenKind::Assign);
        let value = self.parse_expression();

        Statement::VarDecl {
            name,
            mutable,
            is_decl: true,
            type_annotation,
            value,
        }
    }

    /// `var name [: Type] = value` — Go-style mutable declaration. Equivalent to
    /// `let mut name = value` but lighter to read/write. Immutability uses `let`.
    fn parse_var_decl(&mut self) -> Statement {
        self.advance(); // consume 'var'
        let name = self.consume_identifier();

        let type_annotation = if self.current_kind() == TokenKind::Colon {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.expect(TokenKind::Assign);
        let value = self.parse_expression();

        Statement::VarDecl {
            name,
            mutable: true,
            is_decl: true,
            type_annotation,
            value,
        }
    }

    fn parse_bash_assign(&mut self) -> Statement {
        let name = self.consume_identifier();
        self.expect(TokenKind::Assign);
        let value = self.parse_expression();

        Statement::VarDecl {
            name,
            mutable: true,
            is_decl: false,
            type_annotation: None,
            value,
        }
    }

    fn parse_struct_decl(&mut self, is_pub: bool) -> Statement {
        self.advance(); // consume 'struct'
        let name = self.consume_identifier();
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace);

        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }

            let is_field_pub = if self.current_kind() == TokenKind::Pub {
                self.advance();
                true
            } else {
                false
            };

            let field_name = self.consume_identifier();
            self.expect(TokenKind::Colon);
            let type_annotation = self.parse_type_expr();

            fields.push(Field {
                name: field_name,
                type_annotation,
                is_pub: is_field_pub,
            });

            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }

        self.expect(TokenKind::RightBrace);
        Statement::StructDecl { name, fields, is_pub }
    }

    fn parse_enum_decl(&mut self, is_pub: bool) -> Statement {
        self.advance(); // consume 'enum'
        let name = self.consume_identifier();
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace);

        let mut variants = Vec::new();
        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }

            let variant_name = self.consume_identifier();
            let fields = if self.current_kind() == TokenKind::LeftParen {
                self.advance();
                let mut types = Vec::new();
                while self.current_kind() != TokenKind::RightParen && !self.is_at_end() {
                    types.push(self.parse_type_expr());
                    if self.current_kind() == TokenKind::Comma {
                        self.advance();
                    }
                }
                self.expect(TokenKind::RightParen);
                Some(types)
            } else {
                None
            };

            variants.push(EnumVariant {
                name: variant_name,
                fields,
            });

            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }

        self.expect(TokenKind::RightBrace);
        Statement::EnumDecl { name, variants, is_pub }
    }

    fn parse_impl_block(&mut self) -> Statement {
        self.advance(); // consume 'impl'
        let first_name = self.consume_identifier();

        // Check for `impl Trait for Type {}`. `for` lexes as the `For` keyword
        // (not an identifier), so match on the token kind.
        if self.current_kind() == TokenKind::For {
            self.advance(); // consume 'for'
            let type_name = self.consume_identifier();
            let methods = self.parse_methods_block();
            return Statement::ImplBlock {
                type_name,
                trait_name: Some(first_name),
                methods,
            };
        }

        let methods = self.parse_methods_block();
        Statement::ImplBlock {
            type_name: first_name,
            trait_name: None,
            methods,
        }
    }

    /// Parse `trait Name { fn method(self) -> T  ... }`. Each method is a
    /// function declaration that is either a bare signature (no `{ body }`) or
    /// a default implementation (with a body). Signatures keep an empty body.
    fn parse_trait_decl(&mut self, is_pub: bool) -> Statement {
        self.advance(); // consume 'trait'
        let name = self.consume_identifier();
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace);

        let mut methods = Vec::new();
        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }
            if self.current_kind() == TokenKind::Fn {
                let span = self.current_span();
                methods.push(Stmt::new(self.parse_trait_method(), span));
            } else {
                // Tolerate stray tokens inside a trait body without aborting.
                self.advance();
            }
        }

        self.expect(TokenKind::RightBrace);
        Statement::TraitDecl { name, methods, is_pub }
    }

    /// Parse a single trait method: an `fn` signature with an optional default
    /// body. When no `{` follows the signature it is a pure signature and the
    /// resulting `FnDecl` carries an empty body.
    fn parse_trait_method(&mut self) -> Statement {
        self.advance(); // consume 'fn'
        let name = self.consume_identifier();
        self.expect(TokenKind::LeftParen);
        let params = self.parse_params();
        self.expect(TokenKind::RightParen);

        let return_type = if self.current_kind() == TokenKind::RightArrow {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        // A default body, when present, opens with `{` on the same line as the
        // signature; a bare signature is followed by a newline / next `fn`.
        let body = if self.current_kind() == TokenKind::LeftBrace {
            self.parse_block()
        } else {
            Vec::new()
        };

        Statement::FnDecl {
            name,
            params,
            return_type,
            body,
            is_pub: false,
            is_async: false,
        }
    }

    fn parse_methods_block(&mut self) -> Vec<Stmt> {
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace);
        let mut methods = Vec::new();

        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }
            if let Some(stmt) = self.parse_stmt() {
                methods.push(stmt);
            }
        }

        self.expect(TokenKind::RightBrace);
        methods
    }

    fn parse_if(&mut self) -> Statement {
        self.advance(); // consume 'if'
        self.no_struct_literal = true;
        let condition = self.parse_expression();
        self.no_struct_literal = false;
        let then_body = self.parse_block();

        self.skip_newlines();
        let else_body = if self.current_kind() == TokenKind::Else {
            self.advance();
            // Support `else if ...` by parsing a nested if as the else body.
            if self.current_kind() == TokenKind::If {
                Some(vec![self.parse_stmt_from_if()])
            } else {
                Some(self.parse_block())
            }
        } else {
            None
        };

        Statement::If {
            condition,
            then_body,
            else_body,
        }
    }

    /// Helper: parse an `if` appearing in `else if` position, wrapped as a Stmt.
    fn parse_stmt_from_if(&mut self) -> Stmt {
        let span = self.current_span();
        Stmt::new(self.parse_if(), span)
    }

    fn parse_for(&mut self) -> Statement {
        self.advance(); // consume 'for'
        let variable = self.consume_identifier();
        // Expect 'in' keyword
        if self.current_kind() == TokenKind::In {
            self.advance();
        } else {
            self.advance(); // skip whatever is there
        }
        self.no_struct_literal = true;
        let iterable = self.parse_expression();
        self.no_struct_literal = false;
        let body = self.parse_block();

        Statement::For {
            variable,
            iterable,
            body,
        }
    }

    fn parse_while(&mut self) -> Statement {
        self.advance(); // consume 'while'
        self.no_struct_literal = true;
        let condition = self.parse_expression();
        self.no_struct_literal = false;
        let body = self.parse_block();
        Statement::While { condition, body }
    }

    fn parse_spawn(&mut self) -> Statement {
        self.advance(); // consume 'spawn'
        let body = self.parse_block();
        Statement::Spawn { body }
    }

    fn parse_echo(&mut self) -> Statement {
        self.advance(); // consume 'echo'
        // Optional -e flag enables escape interpretation (\n, \t, \r)
        let mut escapes = false;
        if self.current_kind() == TokenKind::Minus
            && self.peek_kind() == Some(TokenKind::Identifier)
            && self.peek_lexeme() == "e"
        {
            self.advance(); // '-'
            self.advance(); // 'e'
            escapes = true;
        }
        let expr = self.parse_expression();
        Statement::Echo { expr, escapes }
    }

    fn parse_return(&mut self) -> Statement {
        self.advance(); // consume 'return'
        if self.current_kind() == TokenKind::Newline
            || self.current_kind() == TokenKind::RightBrace
            || self.is_at_end()
        {
            Statement::Return(None)
        } else {
            Statement::Return(Some(self.parse_expression()))
        }
    }

    fn parse_import(&mut self) -> Statement {
        self.advance(); // consume 'import' or 'use'
        let raw = self.current_lexeme();
        self.advance();
        // Strip surrounding quotes if it's a string literal
        let path = if raw.len() >= 2
            && (raw.starts_with('"') || raw.starts_with('\''))
        {
            raw[1..raw.len() - 1].to_string()
        } else {
            raw
        };

        // Optional `as alias`
        let mut alias = None;
        if self.current_kind() == TokenKind::Identifier && self.current_lexeme() == "as" {
            self.advance(); // consume 'as'
            alias = Some(self.consume_identifier());
        }

        Statement::Import { path, alias }
    }

    fn parse_block(&mut self) -> Vec<Stmt> {
        self.skip_newlines();
        self.expect(TokenKind::LeftBrace);
        let mut stmts = Vec::new();

        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }
            if let Some(stmt) = self.parse_stmt() {
                stmts.push(stmt);
            }
        }

        self.expect(TokenKind::RightBrace);
        stmts
    }

    // --- Expression parsing (precedence climbing) ---

    fn parse_expression(&mut self) -> Expression {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Expression {
        let mut left = self.parse_and();
        while self.current_kind() == TokenKind::PipePipe {
            self.advance();
            let right = self.parse_and();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Or,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_and(&mut self) -> Expression {
        let mut left = self.parse_equality();
        while self.current_kind() == TokenKind::AmpAmp {
            self.advance();
            let right = self.parse_equality();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::And,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_equality(&mut self) -> Expression {
        let mut left = self.parse_comparison();
        loop {
            let op = match self.current_kind() {
                TokenKind::EqualEqual => BinaryOperator::Eq,
                TokenKind::BangEqual => BinaryOperator::Neq,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_comparison(&mut self) -> Expression {
        let mut left = self.parse_addition();
        loop {
            let op = match self.current_kind() {
                TokenKind::Less => BinaryOperator::Lt,
                TokenKind::LessEqual => BinaryOperator::Lte,
                TokenKind::Greater => BinaryOperator::Gt,
                TokenKind::GreaterEqual => BinaryOperator::Gte,
                _ => break,
            };
            self.advance();
            let right = self.parse_addition();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_addition(&mut self) -> Expression {
        let mut left = self.parse_multiplication();
        loop {
            let op = match self.current_kind() {
                TokenKind::Plus => BinaryOperator::Add,
                TokenKind::Minus => BinaryOperator::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplication();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_multiplication(&mut self) -> Expression {
        let mut left = self.parse_unary();
        loop {
            let op = match self.current_kind() {
                TokenKind::Star => BinaryOperator::Mul,
                TokenKind::Slash => BinaryOperator::Div,
                TokenKind::Percent => BinaryOperator::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary();
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_unary(&mut self) -> Expression {
        match self.current_kind() {
            TokenKind::Bang => {
                self.advance();
                Expression::UnaryOp {
                    op: UnaryOperator::Not,
                    operand: Box::new(self.parse_unary()),
                }
            }
            TokenKind::Minus => {
                self.advance();
                Expression::UnaryOp {
                    op: UnaryOperator::Neg,
                    operand: Box::new(self.parse_unary()),
                }
            }
            TokenKind::Amp => {
                self.advance();
                let mutable = if self.current_kind() == TokenKind::Mut {
                    self.advance();
                    true
                } else {
                    false
                };
                let op = if mutable {
                    UnaryOperator::MutRef
                } else {
                    UnaryOperator::Ref
                };
                Expression::UnaryOp {
                    op,
                    operand: Box::new(self.parse_unary()),
                }
            }
            TokenKind::Star => {
                self.advance();
                Expression::UnaryOp {
                    op: UnaryOperator::Deref,
                    operand: Box::new(self.parse_unary()),
                }
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Expression {
        let mut expr = self.parse_primary();

        loop {
            match self.current_kind() {
                TokenKind::LeftParen => {
                    self.advance();
                    let args = self.parse_args();
                    self.expect(TokenKind::RightParen);
                    expr = Expression::FnCall {
                        callee: Box::new(expr),
                        args,
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let field = self.consume_identifier();
                    if self.current_kind() == TokenKind::LeftParen {
                        self.advance();
                        let args = self.parse_args();
                        self.expect(TokenKind::RightParen);
                        expr = Expression::MethodCall {
                            object: Box::new(expr),
                            method: field,
                            args,
                        };
                    } else {
                        expr = Expression::FieldAccess {
                            object: Box::new(expr),
                            field,
                        };
                    }
                }
                TokenKind::LeftBracket => {
                    self.advance();
                    let index = self.parse_expression();
                    self.expect(TokenKind::RightBracket);
                    expr = Expression::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    };
                }
                TokenKind::Pipe => {
                    self.advance();
                    let right = self.parse_postfix();
                    expr = Expression::Pipe {
                        left: Box::new(expr),
                        right: Box::new(right),
                    };
                }
                _ => break,
            }
        }

        expr
    }

    fn parse_primary(&mut self) -> Expression {
        match self.current_kind() {
            TokenKind::IntLiteral => {
                let val = self.current_lexeme().parse::<i64>().unwrap_or(0);
                self.advance();
                Expression::IntLiteral(val)
            }
            TokenKind::FloatLiteral => {
                let val = self.current_lexeme().parse::<f64>().unwrap_or(0.0);
                self.advance();
                Expression::FloatLiteral(val)
            }
            TokenKind::StringLiteral => {
                let raw = self.current_lexeme();
                let inner = if raw.len() >= 2 {
                    raw[1..raw.len() - 1].to_string()
                } else {
                    raw
                };
                // Only unescape quotes and backslashes here so JSON like \" works.
                // Whitespace escapes (\n \t \r) are kept literal and only
                // interpreted by `echo -e` (bash-style).
                let val = unescape_quotes(&inner);
                self.advance();
                Expression::StringLiteral(val)
            }
            TokenKind::True => {
                self.advance();
                Expression::BoolLiteral(true)
            }
            TokenKind::False => {
                self.advance();
                Expression::BoolLiteral(false)
            }
            // Anonymous function / closure expression: `fn(params) { body }`.
            // An optional `-> Type` return annotation is accepted and ignored
            // (closures infer their result), mirroring named-function syntax.
            TokenKind::Fn => {
                self.advance(); // consume 'fn'
                self.expect(TokenKind::LeftParen);
                let params = self.parse_params();
                self.expect(TokenKind::RightParen);
                if self.current_kind() == TokenKind::RightArrow {
                    self.advance();
                    let _ = self.parse_type_expr();
                }
                let body = self.parse_block();
                Expression::Lambda { params, body }
            }
            TokenKind::Variable => {
                let raw = self.current_lexeme();
                let name = if raw.starts_with("${") && raw.ends_with('}') {
                    raw[2..raw.len() - 1].to_string()
                } else if raw.starts_with('$') {
                    raw[1..].to_string()
                } else {
                    raw
                };
                self.advance();
                Expression::Variable(name)
            }
            TokenKind::Identifier => {
                let name = self.current_lexeme();
                self.advance();
                // Struct literal: `Name { field: expr, ... }`, unless we're in a
                // condition header (if/while/for) where `{` starts the body.
                if !self.no_struct_literal && self.current_kind() == TokenKind::LeftBrace {
                    self.parse_struct_init(name)
                } else {
                    Expression::Variable(name)
                }
            }
            TokenKind::LeftParen => {
                self.advance();
                // Inside parentheses, struct literals are unambiguous again.
                let saved = self.no_struct_literal;
                self.no_struct_literal = false;
                let expr = self.parse_expression();
                self.no_struct_literal = saved;
                self.expect(TokenKind::RightParen);
                expr
            }
            TokenKind::LeftBracket => {
                self.advance();
                let saved = self.no_struct_literal;
                self.no_struct_literal = false;
                let mut elements = Vec::new();
                while self.current_kind() != TokenKind::RightBracket && !self.is_at_end() {
                    self.skip_newlines();
                    if self.current_kind() == TokenKind::RightBracket {
                        break;
                    }
                    elements.push(self.parse_expression());
                    if self.current_kind() == TokenKind::Comma {
                        self.advance();
                    }
                    self.skip_newlines();
                }
                self.no_struct_literal = saved;
                self.expect(TokenKind::RightBracket);
                Expression::Array(elements)
            }
            TokenKind::Arrow => {
                // <- chan (channel receive)
                self.advance();
                let channel = self.parse_primary();
                Expression::ChanRecv {
                    channel: Box::new(channel),
                }
            }
            TokenKind::Match => self.parse_match(),
            _ => {
                let lexeme = self.current_lexeme();
                self.error(
                    "E0101",
                    format!("unexpected token in expression: `{:?}`", self.current_kind()),
                    "expected a value: literal, variable, `(`, `[`, or a function call",
                );
                self.advance();
                Expression::Variable(lexeme)
            }
        }
    }

    fn parse_args(&mut self) -> Vec<Expression> {
        let mut args = Vec::new();
        // Arguments are a fresh expression context: struct literals allowed.
        let saved = self.no_struct_literal;
        self.no_struct_literal = false;
        while self.current_kind() != TokenKind::RightParen && !self.is_at_end() {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightParen {
                break;
            }
            args.push(self.parse_expression());
            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        self.no_struct_literal = saved;
        args
    }

    /// Parse `match <subject> { <pattern> => <body>, ... }`.
    fn parse_match(&mut self) -> Expression {
        self.advance(); // consume 'match'
        let saved = self.no_struct_literal;
        self.no_struct_literal = true; // `{` opens arms, not a struct literal
        let subject = self.parse_expression();
        self.no_struct_literal = saved;

        self.expect(TokenKind::LeftBrace);
        let mut arms = Vec::new();
        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }
            let pattern = self.parse_pattern();
            self.expect(TokenKind::FatArrow);
            self.skip_newlines();
            let body = if self.current_kind() == TokenKind::LeftBrace {
                self.parse_block()
            } else {
                // Single-statement arm (expression, echo, assignment, ...).
                match self.parse_stmt() {
                    Some(s) => vec![s],
                    None => Vec::new(),
                }
            };
            arms.push(MatchArm { pattern, body });
            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RightBrace);
        Expression::Match {
            subject: Box::new(subject),
            arms,
        }
    }

    /// Parse one match pattern: `_` (wildcard), a bare identifier (binding),
    /// or any other expression (literal/enum-variant comparison).
    fn parse_pattern(&mut self) -> Pattern {
        if self.current_kind() == TokenKind::Identifier {
            let name = self.current_lexeme();
            // Bare identifier followed by `=>` is a wildcard/binding.
            if self.peek_kind() == Some(TokenKind::FatArrow) {
                self.advance();
                return if name == "_" {
                    Pattern::Wildcard
                } else {
                    Pattern::Variable(name)
                };
            }
        }
        let saved = self.no_struct_literal;
        self.no_struct_literal = true;
        let expr = self.parse_expression();
        self.no_struct_literal = saved;
        Pattern::Literal(expr)
    }

    /// Parse a struct literal body after the type name: `{ field: expr, ... }`.
    /// The current token is the opening `{`.
    fn parse_struct_init(&mut self, name: String) -> Expression {
        self.expect(TokenKind::LeftBrace);
        let saved = self.no_struct_literal;
        self.no_struct_literal = false;
        let mut fields = Vec::new();
        loop {
            self.skip_newlines();
            if self.current_kind() == TokenKind::RightBrace || self.is_at_end() {
                break;
            }
            let field_name = self.consume_identifier();
            // `field: value` — colon is conventional; tolerate `=` too.
            if self.current_kind() == TokenKind::Colon || self.current_kind() == TokenKind::Assign {
                self.advance();
            } else {
                self.error(
                    "E0103",
                    format!("expected `:` after field `{}` in struct literal", field_name),
                    "write fields as `name: value`",
                );
            }
            let value = self.parse_expression();
            fields.push((field_name, value));
            self.skip_newlines();
            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        self.no_struct_literal = saved;
        self.expect(TokenKind::RightBrace);
        Expression::StructInit { name, fields }
    }

    fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        while self.current_kind() != TokenKind::RightParen && !self.is_at_end() {
            let is_mut = if self.current_kind() == TokenKind::Mut {
                self.advance();
                true
            } else {
                false
            };

            let name = self.consume_identifier();

            let type_annotation = if self.current_kind() == TokenKind::Colon {
                self.advance();
                Some(self.parse_type_expr())
            } else {
                None
            };

            params.push(Param {
                name,
                type_annotation,
                is_mut,
            });

            if self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        params
    }

    fn parse_type_expr(&mut self) -> TypeExpr {
        if self.current_kind() == TokenKind::Amp {
            self.advance();
            let mutable = if self.current_kind() == TokenKind::Mut {
                self.advance();
                true
            } else {
                false
            };
            let inner = self.parse_type_expr();
            return TypeExpr::Ref {
                mutable,
                inner: Box::new(inner),
            };
        }

        let name = self.consume_identifier();

        if self.current_kind() == TokenKind::Less {
            self.advance();
            let mut params = Vec::new();
            while self.current_kind() != TokenKind::Greater && !self.is_at_end() {
                params.push(self.parse_type_expr());
                if self.current_kind() == TokenKind::Comma {
                    self.advance();
                }
            }
            self.expect(TokenKind::Greater);
            return TypeExpr::Generic { name, params };
        }

        TypeExpr::Named(name)
    }

    // --- Utility helpers ---

    fn current_kind(&self) -> TokenKind {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].kind.clone()
        } else {
            TokenKind::Eof
        }
    }

    fn peek_kind(&self) -> Option<TokenKind> {
        if self.pos + 1 < self.tokens.len() {
            Some(self.tokens[self.pos + 1].kind.clone())
        } else {
            None
        }
    }

    fn peek_lexeme(&self) -> String {
        if self.pos + 1 < self.tokens.len() {
            self.tokens[self.pos + 1].lexeme.clone()
        } else {
            String::new()
        }
    }

    fn current_lexeme(&self) -> String {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].lexeme.clone()
        } else {
            String::new()
        }
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, kind: TokenKind) {
        if self.current_kind() == kind {
            self.advance();
        } else {
            self.error(
                "E0100",
                format!("expected `{:?}`, found `{:?}`", kind, self.current_kind()),
                format!("insert the expected `{:?}` token", kind),
            );
        }
    }

    fn consume_identifier(&mut self) -> String {
        let name = self.current_lexeme();
        self.advance();
        name
    }

    fn skip_newlines(&mut self) {
        // Newlines and semicolons are both optional statement separators.
        while matches!(self.current_kind(), TokenKind::Newline | TokenKind::Semicolon) {
            self.advance();
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.current_kind() == TokenKind::Eof
    }
}

/// Unescape only quote and backslash sequences (`\"`, `\'`, `\\`).
/// Whitespace escapes like `\n`, `\t`, `\r` are preserved literally so that
/// `echo` prints them as-is and `echo -e` can interpret them later.
fn unescape_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('"') => {
                    out.push('"');
                    chars.next();
                }
                Some('\'') => {
                    out.push('\'');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
