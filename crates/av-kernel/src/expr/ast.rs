//! The AST `docs/adr/005-simulation-kernel.md`'s amendment 2026-09-02 EBNF produces, one node
//! per production, each carrying the byte offset (`pos`) of its own start in the source
//! expression -- used by every [`crate::expr::error::ExprError`] variant that names "the
//! expression position".
//!
//! ## The amended grammar (superseding the ADR's original section 6 EBNF)
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
//! Two structural differences from the pre-amendment grammar this AST used to encode: `ref` no
//! longer carries `['@' time]` itself (moved to the new `postfix` production, so `@time`
//! attaches to *any* primary, including a `call` -- [`Expr::At`]), and `comparison` is a new,
//! non-associative level above `sum` ([`Expr::Compare`], [`CmpOp`]).

use av_cdm::pb::Unit;

/// `time := 'start' | 'end' | number 's' | ident`. The literal grammar's fourth alternative,
/// `ident`, is documented in prose as "an event name resolves to its epoch" -- kept as its own
/// variant here (not folded into a general `Ref`) since `time` is a distinct production from
/// `ref`.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeSpec {
    Start,
    End,
    /// `number 's'` -- an offset in seconds from `Scenario.start_tai_ns`. The `pos` is the
    /// number's own start, for [`crate::expr::error::ExprError::TimeMustBeSeconds`].
    OffsetSeconds { seconds: f64, pos: usize },
    /// `ident` -- an event name; resolved against the run's events at evaluation time.
    EventName { name: String, pos: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
        }
    }
}

/// The amendment's six comparison operators. `comparison := sum [cmp sum]` is non-associative
/// (at most one of these per `comparison` production), so [`Expr::Compare`] never nests another
/// `Expr::Compare` as an operand by construction -- there is no AST shape for `a < b < c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
        }
    }

    pub fn holds(self, lhs: f64, rhs: f64) -> bool {
        match self {
            CmpOp::Lt => lhs < rhs,
            CmpOp::Le => lhs <= rhs,
            CmpOp::Gt => lhs > rhs,
            CmpOp::Ge => lhs >= rhs,
            CmpOp::Eq => lhs == rhs,
            CmpOp::Ne => lhs != rhs,
        }
    }
}

/// One node of the amended grammar's productions. `Unary` covers only `'-' unary` (the
/// grammar's only unary operator); a bare `primary`/`postfix` with no leading `-` is simply not
/// wrapped in a `Unary` node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `number [unit]`. `unit` is [`Unit::Dimensionless`] when no unit suffix was written.
    Number { value: f64, unit: Unit, pos: usize },
    /// `-unary`.
    Neg { expr: Box<Expr>, pos: usize },
    /// `term (('+' | '-') term)*` / `unary (('*' | '/') unary)*`, both left-associative,
    /// folded into one binary-node shape (the AST does not need to remember which grammar
    /// level produced a given operator, only its precedence, already baked in by the parser).
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, pos: usize },
    /// `comparison := sum [cmp sum]`. Non-associative by construction: the parser builds at
    /// most one of these per `comparison` production (see [`CmpOp`]'s doc comment).
    Compare { op: CmpOp, lhs: Box<Expr>, rhs: Box<Expr>, pos: usize },
    /// `postfix := primary ['@' time]`, the `['@' time]` case. Applies to *any* primary,
    /// including a `call` (e.g. `range(a, b)@end`) -- the amendment's fix for the pre-amendment
    /// grammar, whose `call` production had no `@time` suffix at all.
    At { expr: Box<Expr>, time: TimeSpec, pos: usize },
    /// `call := name '(' [expr (',' expr)*] ')'`.
    Call { name: String, args: Vec<Expr>, pos: usize },
    /// `ref := ident ('.' ident)*` -- no longer carries a `time` field (moved to [`Expr::At`]).
    Ref { path: Vec<String>, pos: usize },
}

impl Expr {
    /// The byte offset this node's error messages should cite.
    pub fn pos(&self) -> usize {
        match self {
            Expr::Number { pos, .. }
            | Expr::Neg { pos, .. }
            | Expr::Binary { pos, .. }
            | Expr::Compare { pos, .. }
            | Expr::At { pos, .. }
            | Expr::Call { pos, .. }
            | Expr::Ref { pos, .. } => *pos,
        }
    }
}
