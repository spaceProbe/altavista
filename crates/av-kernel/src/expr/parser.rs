//! Hand-written recursive-descent parser for `docs/adr/005-simulation-kernel.md`'s amendment
//! 2026-09-02 EBNF, implemented **exactly as written** there -- no third-party parser crate
//! (`nom`/`pest`/`chumsky`/`lalrpop`), and no production added beyond what the amendment
//! states. One function per grammar rule:
//!
//! ```text
//! expr       := comparison
//! comparison := sum [('<' | '<=' | '>' | '>=' | '==' | '!=') sum]
//! sum        := term (('+' | '-') term)*
//! term       := unary (('*' | '/') unary)*
//! unary      := '-' unary | postfix
//! postfix    := primary ['@' time]
//! primary    := number [unit] | call | ref | '(' expr ')'
//! call       := name '(' [expr (',' expr)*] ')'
//! ref        := ident ('.' ident)*
//! time       := 'start' | 'end' | number 's' | ident
//! ```
//!
//! `parse_primary` disambiguates `call` from `ref` the only way the grammar allows: both start
//! with an identifier, but `call`'s `name` is a single, undotted identifier immediately
//! followed by `(`; anything else (a `.` follows, or no `(` follows at all) is a `ref`.
//!
//! ## What the pre-amendment grammar got wrong, and how this parser fixes it
//!
//! ADR-005 section 6's *original* EBNF had two defects the amendment (2026-09-02) records and
//! corrects, and this parser implements the corrected grammar, not the original:
//!
//! 1. **`range(<a>, <b>)@time`** (section 6's own reference list) could not be parsed by the
//!    original grammar: `call` had no `['@' time]` in its production -- only `ref` did. The
//!    amendment moves `['@' time]` to a new `postfix` level that wraps *any* primary
//!    ([`crate::expr::ast::Expr::At`]), so `range(a, b)@end` now parses like any other
//!    time-indexed reference.
//! 2. **Comparison operators** (`duration(range(a, b) < 100 m)`, section 6's own worked
//!    example) had no production anywhere in the original `expr`/`term`/`unary`/`primary` --
//!    the lexer did not even tokenize `<`/`>`/`<=`/`>=`/`==`/`!=`. The amendment adds a
//!    non-associative `comparison` level above `sum` (`crate::expr::lexer` now tokenizes all
//!    six operators, and `crate::expr::ast::Expr::Compare` is the AST node).
//!
//! This module -- along with `crate::expr::typecheck`/`crate::expr::eval`, which implement
//! `range`/`duration`'s semantics -- previously carried a disclosure that these two gaps were
//! "escalated, not resolved" (before the lead's amendment). That escalation is resolved now:
//! see this crate's top-level report for what "range"/"duration" mean numerically (a choice the
//! amendment's prose leaves to the implementation, exactly as `crate::expr::eval`'s own module
//! doc comment already discloses its aggregate definitions).

use crate::expr::ast::{BinOp, CmpOp, Expr, TimeSpec};
use crate::expr::error::ExprError;
use crate::expr::lexer::{lex, TokKind, Token};
use crate::expr::units::parse_unit_token;

use av_cdm::pb::Unit;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

fn describe(kind: &TokKind) -> String {
    match kind {
        TokKind::Number(v, u) => format!("number {v}{}", u.map(|u| format!(" {u}")).unwrap_or_default()),
        TokKind::Ident(s) => format!("identifier {s:?}"),
        TokKind::Plus => "'+'".to_string(),
        TokKind::Minus => "'-'".to_string(),
        TokKind::Star => "'*'".to_string(),
        TokKind::Slash => "'/'".to_string(),
        TokKind::LParen => "'('".to_string(),
        TokKind::RParen => "')'".to_string(),
        TokKind::Comma => "','".to_string(),
        TokKind::At => "'@'".to_string(),
        TokKind::Dot => "'.'".to_string(),
        TokKind::Lt => "'<'".to_string(),
        TokKind::Le => "'<='".to_string(),
        TokKind::Gt => "'>'".to_string(),
        TokKind::Ge => "'>='".to_string(),
        TokKind::EqEq => "'=='".to_string(),
        TokKind::Ne => "'!='".to_string(),
        TokKind::Eof => "end of expression".to_string(),
    }
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }
    fn expect(&mut self, want: &TokKind, expected: &'static str) -> Result<Token, ExprError> {
        if std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(want) {
            Ok(self.advance())
        } else if matches!(self.peek().kind, TokKind::Eof) {
            Err(ExprError::UnexpectedEof { expected })
        } else {
            Err(ExprError::UnexpectedToken { pos: self.peek().pos, found: describe(&self.peek().kind), expected })
        }
    }

    // expr := comparison
    fn parse_expr(&mut self) -> Result<Expr, ExprError> {
        self.parse_comparison()
    }

    // comparison := sum [('<' | '<=' | '>' | '>=' | '==' | '!=') sum]  -- non-associative: at
    // most one comparison operator is ever consumed here, so `a < b < c` leaves the second
    // operator as unparsed trailing input (a parse error at the top level, per the amendment's
    // "it is not associative, so `a < b < c` is a parse error").
    fn parse_comparison(&mut self) -> Result<Expr, ExprError> {
        let lhs = self.parse_sum()?;
        let op = match &self.peek().kind {
            TokKind::Lt => CmpOp::Lt,
            TokKind::Le => CmpOp::Le,
            TokKind::Gt => CmpOp::Gt,
            TokKind::Ge => CmpOp::Ge,
            TokKind::EqEq => CmpOp::Eq,
            TokKind::Ne => CmpOp::Ne,
            _ => return Ok(lhs),
        };
        let pos = self.advance().pos;
        let rhs = self.parse_sum()?;
        Ok(Expr::Compare { op, lhs: Box::new(lhs), rhs: Box::new(rhs), pos })
    }

    // sum := term (('+' | '-') term)*
    fn parse_sum(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_term()?;
        loop {
            let op = match &self.peek().kind {
                TokKind::Plus => BinOp::Add,
                TokKind::Minus => BinOp::Sub,
                _ => break,
            };
            let pos = self.advance().pos;
            let rhs = self.parse_term()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), pos };
        }
        Ok(lhs)
    }

    // term := unary (('*' | '/') unary)*
    fn parse_term(&mut self) -> Result<Expr, ExprError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match &self.peek().kind {
                TokKind::Star => BinOp::Mul,
                TokKind::Slash => BinOp::Div,
                _ => break,
            };
            let pos = self.advance().pos;
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs), pos };
        }
        Ok(lhs)
    }

    // unary := '-' unary | postfix
    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if matches!(self.peek().kind, TokKind::Minus) {
            let pos = self.advance().pos;
            let inner = self.parse_unary()?;
            return Ok(Expr::Neg { expr: Box::new(inner), pos });
        }
        self.parse_postfix()
    }

    // postfix := primary ['@' time] -- applies to any primary, call included (the amendment's
    // fix: `range(a, b)@end` now parses the same way `entity.sat.pos_x@start` does).
    fn parse_postfix(&mut self) -> Result<Expr, ExprError> {
        let primary = self.parse_primary()?;
        if matches!(self.peek().kind, TokKind::At) {
            let pos = self.advance().pos;
            let time = self.parse_time()?;
            return Ok(Expr::At { expr: Box::new(primary), time, pos });
        }
        Ok(primary)
    }

    // primary := number [unit] | call | ref | '(' expr ')'
    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokKind::Number(value, unit_tok) => {
                self.advance();
                let unit = match unit_tok {
                    Some(t) => parse_unit_token(t).expect("lexer only ever emits a unit token from units::UNIT_TOKENS"),
                    None => Unit::Dimensionless,
                };
                Ok(Expr::Number { value, unit, pos: tok.pos })
            }
            TokKind::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&TokKind::RParen, "')'")?;
                Ok(inner)
            }
            TokKind::Ident(_) => self.parse_call_or_ref(),
            // The source ended before this `primary` (already committed to by the preceding
            // operator/`(`/`,`) could start -- `UnexpectedEof`, matching this variant's own
            // documented contract ("an operand after an operator"), not `UnexpectedToken`
            // (which would self-describe as `found: "end of expression"`, the wrong shape for
            // "there is no more input").
            TokKind::Eof => Err(ExprError::UnexpectedEof { expected: "a number, '(', or an identifier" }),
            _ => Err(ExprError::UnexpectedToken { pos: tok.pos, found: describe(&tok.kind), expected: "a number, '(', or an identifier" }),
        }
    }

    /// `call := name '(' [expr (',' expr)*] ')'` vs. `ref := ident ('.' ident)*`: both start
    /// with one identifier; it is a `call` only when that identifier is *immediately* followed
    /// by `(` (no `.` in between) -- see the module doc comment. Neither production carries
    /// `@time` any more (that moved to `postfix`, which wraps whichever of the two this
    /// function returns).
    fn parse_call_or_ref(&mut self) -> Result<Expr, ExprError> {
        let first = self.advance();
        let name = match first.kind {
            TokKind::Ident(s) => s,
            _ => unreachable!("caller only invokes this on an Ident token"),
        };
        if matches!(self.peek().kind, TokKind::LParen) {
            self.advance();
            let mut args = Vec::new();
            if !matches!(self.peek().kind, TokKind::RParen) {
                args.push(self.parse_expr()?);
                while matches!(self.peek().kind, TokKind::Comma) {
                    self.advance();
                    args.push(self.parse_expr()?);
                }
            }
            self.expect(&TokKind::RParen, "')'")?;
            return Ok(Expr::Call { name, args, pos: first.pos });
        }
        let mut path = vec![name];
        while matches!(self.peek().kind, TokKind::Dot) {
            self.advance();
            match self.peek().kind.clone() {
                TokKind::Ident(s) => {
                    self.advance();
                    path.push(s);
                }
                _ => return Err(ExprError::UnexpectedToken { pos: self.peek().pos, found: describe(&self.peek().kind), expected: "an identifier after '.'" }),
            }
        }
        Ok(Expr::Ref { path, pos: first.pos })
    }

    // time := 'start' | 'end' | number 's' | ident
    fn parse_time(&mut self) -> Result<TimeSpec, ExprError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokKind::Ident(s) => {
                self.advance();
                match s.as_str() {
                    "start" => Ok(TimeSpec::Start),
                    "end" => Ok(TimeSpec::End),
                    _ => Ok(TimeSpec::EventName { name: s, pos: tok.pos }),
                }
            }
            TokKind::Number(value, unit_tok) => {
                self.advance();
                let unit = match unit_tok {
                    Some(t) => parse_unit_token(t).expect("lexer only ever emits a unit token from units::UNIT_TOKENS"),
                    None => Unit::Dimensionless,
                };
                if unit != Unit::Second {
                    return Err(ExprError::TimeMustBeSeconds { unit, pos: tok.pos });
                }
                Ok(TimeSpec::OffsetSeconds { seconds: value, pos: tok.pos })
            }
            _ => Err(ExprError::UnexpectedToken { pos: tok.pos, found: describe(&tok.kind), expected: "'start', 'end', a number of seconds, or an event name" }),
        }
    }
}

/// Parse a complete amended-grammar expression: the whole string must be exactly one `expr`,
/// with nothing left over ([`ExprError::TrailingInput`] otherwise -- e.g. `a < b < c`, since
/// `comparison` is non-associative and consumes only the first `cmp sum`).
pub fn parse(src: &str) -> Result<Expr, ExprError> {
    let tokens = lex(src)?;
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_expr()?;
    if !matches!(p.peek().kind, TokKind::Eof) {
        return Err(ExprError::TrailingInput { pos: p.peek().pos, found: describe(&p.peek().kind) });
    }
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::ast::Expr::*;

    #[test]
    fn parses_arithmetic_with_precedence_and_left_associativity() {
        // 1 + 2 * 3 - 4 / 2 == 1 + (2*3) - (4/2), left-to-right at each precedence level.
        let e = parse("1 + 2 * 3 - 4 / 2").unwrap();
        assert!(matches!(e, Binary { op: BinOp::Sub, .. }));
    }

    #[test]
    fn parses_unary_minus_and_parens() {
        let e = parse("-(1 + 2)").unwrap();
        assert!(matches!(e, Neg { .. }));
    }

    #[test]
    fn parses_a_number_with_unit() {
        let e = parse("100 m").unwrap();
        assert!(matches!(e, Number { value: 100.0, unit: Unit::Meter, .. }));
    }

    #[test]
    fn disambiguates_call_from_dotted_ref() {
        assert!(matches!(parse("mean(x)").unwrap(), Call { .. }));
        assert!(matches!(parse("entity.leo.pos_x").unwrap(), Ref { .. }));
    }

    #[test]
    fn parses_a_ref_with_at_time_forms() {
        let At { expr, time, .. } = parse("entity.leo.pos_x@start").unwrap() else { panic!() };
        assert!(matches!(*expr, Ref { ref path, .. } if path == &["entity", "leo", "pos_x"]));
        assert_eq!(time, TimeSpec::Start);

        let At { time, .. } = parse("entity.leo.pos_x@end").unwrap() else { panic!() };
        assert_eq!(time, TimeSpec::End);

        let At { time, .. } = parse("entity.leo.pos_x@100 s").unwrap() else { panic!() };
        assert!(matches!(time, TimeSpec::OffsetSeconds { seconds, .. } if seconds == 100.0));

        let At { time, .. } = parse("entity.leo.pos_x@apogee_burn").unwrap() else { panic!() };
        assert!(matches!(time, TimeSpec::EventName { name, .. } if name == "apogee_burn"));
    }

    #[test]
    fn parses_nested_calls_and_multiple_arguments() {
        let e = parse("count(event.maneuver)").unwrap();
        assert!(matches!(e, Call { ref name, ref args, .. } if name == "count" && args.len() == 1));
    }

    #[test]
    fn parses_at_time_on_a_call_the_amendments_fix_for_the_pre_amendment_gap() {
        // range(a, b)@end: `postfix` wraps any primary, calls included -- unparseable under the
        // pre-amendment grammar (`call` had no `['@' time]`), well-formed under the amendment.
        let e = parse("range(a, b)@end").unwrap();
        let At { expr, time, .. } = e else { panic!("{e:?}") };
        assert!(matches!(*expr, Call { ref name, .. } if name == "range"));
        assert_eq!(time, TimeSpec::End);
    }

    #[test]
    fn parses_the_sec_6_worked_example_comparison_and_duration() {
        // duration(range(a, b) < 100 m): unparseable under the pre-amendment grammar (no
        // comparison production at all); the amendment's whole point.
        let e = parse("duration(range(a, b) < 100 m)").unwrap();
        let Call { name, args, .. } = e else { panic!("{e:?}") };
        assert_eq!(name, "duration");
        assert_eq!(args.len(), 1);
        assert!(matches!(args[0], Compare { op: CmpOp::Lt, .. }));
    }

    #[test]
    fn comparison_is_non_associative_a_lt_b_lt_c_is_a_parse_error() {
        let err = parse("1 < 2 < 3").unwrap_err();
        assert!(matches!(err, ExprError::TrailingInput { .. }), "{err:?}");
    }

    #[test]
    fn parses_every_comparison_operator() {
        for (src, want) in [("1 < 2", CmpOp::Lt), ("1 <= 2", CmpOp::Le), ("1 > 2", CmpOp::Gt), ("1 >= 2", CmpOp::Ge), ("1 == 2", CmpOp::Eq), ("1 != 2", CmpOp::Ne)] {
            let e = parse(src).unwrap();
            assert!(matches!(e, Compare { op, .. } if op == want), "{src}: {e:?}");
        }
    }

    #[test]
    fn reports_position_on_a_dangling_operator() {
        let err = parse("1 +").unwrap_err();
        assert!(matches!(err, ExprError::UnexpectedEof { .. }), "{err:?}");
    }
}
