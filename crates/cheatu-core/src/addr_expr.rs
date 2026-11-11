//! Address expressions: one line that names an address the same way in the
//! GUI, the CLI and the table file, e.g. `[["dmhost"+0x1a2b30]+0x48]+0x10`.
//!
//! ```text
//! expr   := term (('+' | '-') term)*
//! term   := number | module | deref | aob | '(' expr ')'
//! number := '0x' hexdigits | decdigits
//! module := '"' name '"'
//! deref  := '[' expr ']'
//! aob    := 'aob(' '"' pattern '"' (',' '"' name '"')? ')'
//! ```

use std::fmt;

use crate::scan::parse_aob;

/// Longest accepted expression text, in characters. Together with the nesting
/// limit this bounds the depth of the parsed tree.
pub const MAX_EXPR_LEN: usize = 1024;

/// A node of a parsed address expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// A plain number (an offset, or an absolute address).
    Num(u64),
    /// The base address of the named module.
    Module(String),
    /// Read 8 bytes at the inner address (little-endian).
    Deref(Box<Expr>),
    /// The unique match of a byte pattern, optionally limited to one module.
    Aob {
        pattern: String,
        module: Option<String>,
    },
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
}

/// A parsed address expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrExpr(pub Expr);

/// A syntax error with the 1-based character column where it was detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "column {}: {}", self.column, self.message)
    }
}

impl std::error::Error for ParseError {}

impl AddrExpr {
    /// Parse an expression. Whitespace between tokens is ignored.
    pub fn parse(s: &str) -> Result<AddrExpr, ParseError> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() > MAX_EXPR_LEN {
            return Err(ParseError {
                column: MAX_EXPR_LEN + 1,
                message: format!("expression too long (over {MAX_EXPR_LEN} characters)"),
            });
        }
        let mut p = Parser {
            chars,
            pos: 0,
            depth: 0,
        };
        let expr = p.expr()?;
        p.skip_ws();
        if let Some(c) = p.peek() {
            return Err(p.error(format!("unexpected `{c}`")));
        }
        let expr = AddrExpr(expr);
        // Whatever parses must survive a save and reload, so the canonical
        // text has to fit the input limit too.
        if expr.to_string().chars().count() > MAX_EXPR_LEN {
            return Err(ParseError {
                column: 1,
                message: format!(
                    "expression too long in canonical form (over {MAX_EXPR_LEN} characters)"
                ),
            });
        }
        Ok(expr)
    }

    /// True when no plain number is used as an address: every base is a
    /// module or `aob(...)`, numbers only appear as offsets.
    pub fn is_static(&self) -> bool {
        is_static(&self.0)
    }
}

fn is_static(e: &Expr) -> bool {
    match e {
        Expr::Num(_) => false,
        Expr::Module(_) | Expr::Aob { .. } => true,
        Expr::Deref(inner) => is_static(inner),
        Expr::Add(l, _) | Expr::Sub(l, _) => is_static(l),
    }
}

impl fmt::Display for AddrExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_expr(&self.0, f)
    }
}

fn write_expr(e: &Expr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match e {
        Expr::Num(n) => write!(f, "{n:#x}"),
        Expr::Module(name) => write!(f, "\"{name}\""),
        Expr::Deref(inner) => {
            f.write_str("[")?;
            write_expr(inner, f)?;
            f.write_str("]")
        }
        Expr::Aob { pattern, module } => {
            write!(f, "aob(\"{pattern}\"")?;
            if let Some(m) = module {
                write!(f, ", \"{m}\"")?;
            }
            f.write_str(")")
        }
        Expr::Add(l, r) => write_binary(l, '+', r, f),
        Expr::Sub(l, r) => write_binary(l, '-', r, f),
    }
}

// `+`/`-` are left-associative, so only a nested right operand needs parens.
fn write_binary(l: &Expr, op: char, r: &Expr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write_expr(l, f)?;
    write!(f, "{op}")?;
    if matches!(r, Expr::Add(..) | Expr::Sub(..)) {
        f.write_str("(")?;
        write_expr(r, f)?;
        f.write_str(")")
    } else {
        write_expr(r, f)
    }
}

/// Deepest allowed nesting of `[...]` and `(...)`, so hostile input fails
/// with an error instead of overflowing the stack.
const MAX_DEPTH: usize = 64;

struct Parser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// Error at the current position (1-based column).
    fn error(&self, message: String) -> ParseError {
        ParseError {
            column: self.pos + 1,
            message,
        }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), ParseError> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(format!("expected `{c}`")))
        }
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.term()?;
        loop {
            self.skip_ws();
            let add = match self.peek() {
                Some('+') => true,
                Some('-') => false,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = Box::new(self.term()?);
            left = if add {
                Expr::Add(Box::new(left), right)
            } else {
                Expr::Sub(Box::new(left), right)
            };
        }
    }

    fn term(&mut self) -> Result<Expr, ParseError> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c.is_ascii_digit() => self.number(),
            Some('"') => Ok(Expr::Module(self.quoted("module name")?)),
            Some('[') => {
                let inner = self.nested(']')?;
                Ok(Expr::Deref(Box::new(inner)))
            }
            Some('(') => self.nested(')'),
            Some(c) if c.is_alphabetic() || c == '_' => self.function(),
            _ => Err(self.error("expected a number, module, `[` or aob(".to_string())),
        }
    }

    /// Parse `expr close` after an opening bracket at the current position.
    fn nested(&mut self, close: char) -> Result<Expr, ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("expression nested too deeply".to_string()));
        }
        self.depth += 1;
        self.pos += 1;
        let inner = self.expr()?;
        self.expect(close)?;
        self.depth -= 1;
        Ok(inner)
    }

    fn number(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let hex =
            self.peek() == Some('0') && matches!(self.chars.get(self.pos + 1), Some('x' | 'X'));
        let (radix, digits_from) = if hex { (16, start + 2) } else { (10, start) };
        let mut end = digits_from;
        while self.chars.get(end).is_some_and(|c| c.is_digit(radix)) {
            end += 1;
        }
        let column = start + 1;
        if end == digits_from {
            return Err(ParseError {
                column,
                message: "expected hex digits after `0x`".to_string(),
            });
        }
        let text: String = self.chars[digits_from..end].iter().collect();
        let value = u64::from_str_radix(&text, radix).map_err(|_| ParseError {
            column,
            message: "number does not fit in 64 bits".to_string(),
        })?;
        self.pos = end;
        Ok(Expr::Num(value))
    }

    /// A `"..."` string; the name may hold anything except `"`.
    fn quoted(&mut self, what: &str) -> Result<String, ParseError> {
        let open = self.pos;
        self.pos += 1;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '"' {
                let text: String = self.chars[start..self.pos].iter().collect();
                self.pos += 1;
                if text.is_empty() {
                    return Err(ParseError {
                        column: open + 1,
                        message: format!("empty {what}"),
                    });
                }
                return Ok(text);
            }
            self.pos += 1;
        }
        Err(self.error("expected closing `\"`".to_string()))
    }

    fn function(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        let ident: String = self.chars[start..self.pos].iter().collect();
        if ident != "aob" {
            return Err(ParseError {
                column: start + 1,
                message: format!("unknown function `{ident}`"),
            });
        }
        self.expect('(')?;
        self.skip_ws();
        if self.peek() != Some('"') {
            return Err(self.error("expected a quoted byte pattern".to_string()));
        }
        let pat_col = self.pos + 1;
        let pattern = self.quoted("byte pattern")?;
        if !parse_aob(&pattern).is_some_and(|p| !p.is_empty()) {
            return Err(ParseError {
                column: pat_col,
                message: "invalid byte pattern".to_string(),
            });
        }
        self.skip_ws();
        let module = if self.peek() == Some(',') {
            self.pos += 1;
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(self.error("expected a quoted module name".to_string()));
            }
            Some(self.quoted("module name")?)
        } else {
            None
        };
        self.expect(')')?;
        Ok(Expr::Aob { pattern, module })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn num(n: u64) -> Box<Expr> {
        Box::new(Expr::Num(n))
    }
    fn module(s: &str) -> Box<Expr> {
        Box::new(Expr::Module(s.to_string()))
    }
    fn p(s: &str) -> AddrExpr {
        AddrExpr::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }
    fn err(s: &str) -> ParseError {
        AddrExpr::parse(s).unwrap_err()
    }

    #[test]
    fn parses_module_offset() {
        assert_eq!(
            p("\"dmhost\"+0x1a2b30").0,
            Expr::Add(module("dmhost"), num(0x1a2b30))
        );
    }

    #[test]
    fn parses_pointer_chain() {
        let s = "[[\"dmhost\"+0x1a2b30]+0x48]+0x6337e134";
        assert_eq!(p(s).to_string(), s);
    }

    #[test]
    fn parses_aob_with_and_without_module() {
        assert_eq!(
            p("aob(\"48 8B ?? 89\")").0,
            Expr::Aob {
                pattern: "48 8B ?? 89".into(),
                module: None
            }
        );
        let s = "aob(\"48 8B ?? 89\", \"dmhost\")+0x10";
        assert_eq!(
            p(s).0,
            Expr::Add(
                Box::new(Expr::Aob {
                    pattern: "48 8B ?? 89".into(),
                    module: Some("dmhost".into())
                }),
                num(0x10)
            )
        );
        assert_eq!(p(s).to_string(), s);
    }

    #[test]
    fn decimal_without_prefix() {
        assert_eq!(p("10").0, Expr::Num(10));
        assert_eq!(p("0x10").0, Expr::Num(16));
    }

    #[test]
    fn minus_offset() {
        assert_eq!(p("\"m\"-0x8").0, Expr::Sub(module("m"), num(8)));
        assert_eq!(p("\"m\"-0x8").to_string(), "\"m\"-0x8");
    }

    #[test]
    fn left_associative_and_parens() {
        assert_eq!(
            p("1+2-3").0,
            Expr::Sub(Box::new(Expr::Add(num(1), num(2))), num(3))
        );
        assert_eq!(
            p("1+(2-3)").0,
            Expr::Add(num(1), Box::new(Expr::Sub(num(2), num(3))))
        );
        assert_eq!(p("1+(2-3)").to_string(), "0x1+(0x2-0x3)");
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(p("[ \"m\" + 0x10 ] + 4").to_string(), "[\"m\"+0x10]+0x4");
    }

    #[test]
    fn names_with_spaces_and_unicode() {
        for s in ["\"My Game\"+0x10", "\"ゼルダ\"+0x10"] {
            assert_eq!(p(s).to_string(), s);
        }
    }

    #[test]
    fn parse_errors_carry_columns() {
        let e = err("[\"m\"+0x10");
        assert_eq!(e.column, 10);
        assert!(e.message.contains("expected `]`"), "{}", e.message);
        assert_eq!(e.to_string(), format!("column 10: {}", e.message));

        let e = err("\"m\"+");
        assert_eq!(e.column, 5);
        assert!(
            e.message.contains("expected a number, module, `[` or aob("),
            "{}",
            e.message
        );

        let e = err("foo(1)");
        assert_eq!(e.column, 1);
        assert!(e.message.contains("unknown function `foo`"));

        assert!(AddrExpr::parse("\"a\"b\"").is_err());
        assert!(AddrExpr::parse("").is_err());
        assert!(AddrExpr::parse("\"\"+1").is_err());
        assert!(AddrExpr::parse("(1+2").is_err());
        assert!(AddrExpr::parse("aob(\"zz\")").is_err());
        assert!(AddrExpr::parse("1 2").is_err());
        assert!(AddrExpr::parse("0x").is_err());
    }

    #[test]
    fn overflow_is_a_parse_error() {
        assert_eq!(p("0xffffffffffffffff").0, Expr::Num(u64::MAX));
        let e = err("1+0x10000000000000000");
        assert_eq!(e.column, 3);
        assert!(AddrExpr::parse("18446744073709551616").is_err());
        assert_eq!(p("18446744073709551615").0, Expr::Num(u64::MAX));
    }

    #[test]
    fn nesting_depth_is_limited() {
        let deep = "[".repeat(1000);
        let e = err(&deep);
        assert_eq!(e.column, 65);
        assert_eq!(e.message, "expression nested too deeply");
        let e = err(&"(".repeat(1000));
        assert_eq!(e.message, "expression nested too deeply");

        let ok = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        assert!(AddrExpr::parse(&ok).is_ok());
        let too = format!("{}1{}", "[".repeat(65), "]".repeat(65));
        assert_eq!(err(&too).column, 65);
    }

    #[test]
    fn length_is_capped() {
        let e = err(&"1".repeat(1025));
        assert_eq!(e.column, 1025);
        assert_eq!(e.message, "expression too long (over 1024 characters)");
        // Counted in characters, not bytes.
        assert!(AddrExpr::parse(&format!("\"{}\"", "ゼ".repeat(1022))).is_ok());

        // Fits as typed (1024 chars) but not once `0x` prefixes are added.
        let chain = format!("{} ", "1+".repeat(511) + "1");
        assert_eq!(chain.chars().count(), 1024);
        let e = err(&chain);
        assert_eq!(e.column, 1);
        assert_eq!(
            e.message,
            "expression too long in canonical form (over 1024 characters)"
        );

        // Canonical form that fits parses and round-trips.
        let fits = "0x1+".repeat(255) + "0x1";
        assert_eq!(fits.chars().count(), 1023);
        let parsed = p(&fits);
        assert_eq!(parsed.to_string(), fits);
        assert_eq!(p(&parsed.to_string()), parsed);
    }

    #[test]
    fn is_static_rules() {
        assert!(p("[\"m\"+0x10]+0x4").is_static());
        assert!(p("aob(\"90\", \"m\")+0x2").is_static());
        assert!(!p("0x7ffe0000+0x4").is_static());
        assert!(!p("[0x7ffe0000]+0x4").is_static());
        assert!(!p("0x10").is_static());
    }

    fn arb_expr() -> impl Strategy<Value = Expr> {
        let leaf = prop_oneof![
            any::<u64>().prop_map(Expr::Num),
            "[^\"]{1,8}".prop_map(Expr::Module),
            (
                proptest::collection::vec(prop_oneof!["[0-9A-F]{2}", Just("??".to_string())], 1..5),
                proptest::option::of("[^\"]{1,8}")
            )
                .prop_map(|(toks, module)| Expr::Aob {
                    pattern: toks.join(" "),
                    module
                }),
        ];
        leaf.prop_recursive(4, 32, 2, |inner| {
            prop_oneof![
                inner.clone().prop_map(|e| Expr::Deref(Box::new(e))),
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| Expr::Add(Box::new(a), Box::new(b))),
                (inner.clone(), inner).prop_map(|(a, b)| Expr::Sub(Box::new(a), Box::new(b))),
            ]
        })
    }

    proptest! {
        #[test]
        fn display_then_parse_round_trips(e in arb_expr()) {
            let text = AddrExpr(e.clone()).to_string();
            let parsed = AddrExpr::parse(&text).unwrap_or_else(|er| panic!("{text}: {er}"));
            prop_assert_eq!(parsed.0, e);
        }
    }
}
