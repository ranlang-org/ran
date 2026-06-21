//! Lexer module - Tokenizes .ran source code into a stream of tokens.
//! Supports bash-style syntax: $variables, echo, pipes, string interpolation.

pub use super::token::{Span, Token, TokenKind};

/// Tokenize source code into a vector of tokens.
pub fn tokenize(source: &str) -> Vec<Token> {
    let mut lexer = Lexer::new(source);
    lexer.scan_all()
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    at_line_start: bool,
}

impl Lexer {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            at_line_start: true,
        }
    }

    fn scan_all(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();

        while !self.is_at_end() {
            self.skip_whitespace();
            if self.is_at_end() {
                break;
            }

            let token = self.scan_token();
            if let Some(tok) = token {
                // A non-newline token means we are no longer at line start
                if tok.kind != TokenKind::Newline {
                    self.at_line_start = false;
                } else {
                    self.at_line_start = true;
                }
                tokens.push(tok);
            }
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            lexeme: String::new(),
            span: Span {
                line: self.line,
                col: self.col,
                start: self.pos,
                end: self.pos,
            },
        });

        tokens
    }

    fn scan_token(&mut self) -> Option<Token> {
        let start = self.pos;
        let start_line = self.line;
        let start_col = self.col;
        let ch = self.advance();

        let kind = match ch {
            // Comments
            '#' => {
                if start_col == 1 && self.peek() == '!' {
                    // Shebang line: #!/usr/bin/env ran
                    while !self.is_at_end() && self.peek() != '\n' {
                        self.advance();
                    }
                    return None;
                }
                // Skip until end of line (bash-style comment)
                while !self.is_at_end() && self.peek() != '\n' {
                    self.advance();
                }
                return None;
            }

            // Strings
            '"' => self.scan_string('"'),
            '\'' => self.scan_string('\''),

            // Variable reference $name
            '$' => self.scan_variable(),

            // Operators & punctuation
            '=' => {
                if self.match_char('=') {
                    TokenKind::EqualEqual
                } else if self.match_char('>') {
                    TokenKind::FatArrow // => match arm
                } else {
                    TokenKind::Assign
                }
            }
            '!' => {
                if self.match_char('=') {
                    TokenKind::BangEqual
                } else {
                    TokenKind::Bang
                }
            }
            '<' => {
                if self.match_char('=') {
                    TokenKind::LessEqual
                } else if self.match_char('-') {
                    TokenKind::Arrow // <- channel receive
                } else {
                    TokenKind::Less
                }
            }
            '>' => {
                if self.match_char('=') {
                    TokenKind::GreaterEqual
                } else {
                    TokenKind::Greater
                }
            }
            '+' => TokenKind::Plus,
            '-' => {
                if self.match_char('>') {
                    TokenKind::RightArrow // -> return type
                } else {
                    TokenKind::Minus
                }
            }
            '*' => TokenKind::Star,
            '/' => {
                if self.match_char('/') {
                    // C++ style line comment: // ...
                    while !self.is_at_end() && self.peek() != '\n' {
                        self.advance();
                    }
                    return None;
                } else if self.match_char('*') {
                    // C style block comment: /* ... */ (supports nesting)
                    let mut depth = 1;
                    while !self.is_at_end() && depth > 0 {
                        let c = self.advance();
                        if c == '\n' {
                            self.line += 1;
                            self.col = 1;
                        } else if c == '/' && self.peek() == '*' {
                            self.advance();
                            depth += 1;
                        } else if c == '*' && self.peek() == '/' {
                            self.advance();
                            depth -= 1;
                        }
                    }
                    return None;
                } else {
                    TokenKind::Slash
                }
            }
            '%' => TokenKind::Percent,
            '|' => {
                if self.match_char('|') {
                    TokenKind::PipePipe
                } else {
                    TokenKind::Pipe
                }
            }
            '&' => {
                if self.match_char('&') {
                    TokenKind::AmpAmp
                } else {
                    TokenKind::Amp
                }
            }
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '{' => TokenKind::LeftBrace,
            '}' => TokenKind::RightBrace,
            '[' => TokenKind::LeftBracket,
            ']' => TokenKind::RightBracket,
            ',' => TokenKind::Comma,
            '.' => TokenKind::Dot,
            ':' => TokenKind::Colon,
            ';' => {
                // A line that STARTS with ';' is a comment (whole line skipped).
                // A ';' after other content is a statement separator.
                if self.at_line_start {
                    while !self.is_at_end() && self.peek() != '\n' {
                        self.advance();
                    }
                    return None;
                }
                TokenKind::Semicolon
            }
            '\n' => {
                self.line += 1;
                self.col = 1;
                TokenKind::Newline
            }

            // Numbers
            c if c.is_ascii_digit() => self.scan_number(),

            // Identifiers & keywords
            c if c.is_alphabetic() || c == '_' => self.scan_identifier(start),

            _ => {
                eprintln!(
                    "ran: unexpected character '{}' at {}:{}",
                    ch, start_line, start_col
                );
                return None;
            }
        };

        let lexeme: String = self.chars[start..self.pos].iter().collect();

        Some(Token {
            kind,
            lexeme,
            span: Span {
                line: start_line,
                col: start_col,
                start,
                end: self.pos,
            },
        })
    }

    fn scan_string(&mut self, quote: char) -> TokenKind {
        while !self.is_at_end() && self.peek() != quote {
            if self.peek() == '\n' {
                self.line += 1;
                self.col = 1;
            }
            if self.peek() == '\\' {
                self.advance(); // skip escape char
            }
            self.advance();
        }

        if !self.is_at_end() {
            self.advance(); // closing quote
        }

        TokenKind::StringLiteral
    }

    fn scan_variable(&mut self) -> TokenKind {
        if self.peek() == '{' {
            self.advance(); // skip {
            while !self.is_at_end() && self.peek() != '}' {
                self.advance();
            }
            if !self.is_at_end() {
                self.advance(); // skip }
            }
        } else {
            while !self.is_at_end() && (self.peek().is_alphanumeric() || self.peek() == '_') {
                self.advance();
            }
        }
        TokenKind::Variable
    }

    fn scan_number(&mut self) -> TokenKind {
        while !self.is_at_end() && self.peek().is_ascii_digit() {
            self.advance();
        }

        if !self.is_at_end() && self.peek() == '.' {
            self.advance();
            while !self.is_at_end() && self.peek().is_ascii_digit() {
                self.advance();
            }
            return TokenKind::FloatLiteral;
        }

        TokenKind::IntLiteral
    }

    fn scan_identifier(&mut self, start: usize) -> TokenKind {
        while !self.is_at_end() && (self.peek().is_alphanumeric() || self.peek() == '_') {
            self.advance();
        }

        let word: String = self.chars[start..self.pos].iter().collect();
        Self::keyword_or_ident(&word)
    }

    fn keyword_or_ident(word: &str) -> TokenKind {
        match word {
            "fn" => TokenKind::Fn,
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "while" => TokenKind::While,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "return" => TokenKind::Return,
            "spawn" => TokenKind::Spawn,
            "chan" => TokenKind::Chan,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "impl" => TokenKind::Impl,
            "trait" => TokenKind::Trait,
            "pub" => TokenKind::Pub,
            "use" => TokenKind::Use,
            "mod" => TokenKind::Mod,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "echo" => TokenKind::Echo,
            "import" => TokenKind::Import,
            "export" => TokenKind::Export,
            "async" => TokenKind::Async,
            "await" => TokenKind::Await,
            "match" => TokenKind::Match,
            "type" => TokenKind::Type,
            "const" => TokenKind::Const,
            "unsafe" => TokenKind::Unsafe,
            _ => TokenKind::Identifier,
        }
    }

    // --- Helpers ---

    fn advance(&mut self) -> char {
        let ch = self.chars[self.pos];
        self.pos += 1;
        self.col += 1;
        ch
    }

    fn peek(&self) -> char {
        if self.is_at_end() {
            '\0'
        } else {
            self.chars[self.pos]
        }
    }

    fn match_char(&mut self, expected: char) -> bool {
        if self.is_at_end() || self.chars[self.pos] != expected {
            return false;
        }
        self.pos += 1;
        self.col += 1;
        true
    }

    fn skip_whitespace(&mut self) {
        while !self.is_at_end() {
            match self.peek() {
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                _ => break,
            }
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }
}
