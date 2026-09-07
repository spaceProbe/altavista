//! ADR-005 sec 6 (as corrected by the lead's amendment 2026-09-02, `docs/adr/
//! 005-simulation-kernel.md`): the expression language `Objective.expression`/
//! `MeasureOfEffectiveness.expression` are written in -- "a small, **total, deterministic,
//! unit-checked** expression language evaluated after a run over its products."
//!
//! ## Pipeline
//!
//! [`parser::parse`] (hand-written recursive-descent lexer + parser, no third-party parser
//! crate) produces an [`ast::Expr`]. [`typecheck::check`] propagates units through it against a
//! run's *declared* shape only (never touching sample data) -- ADR-005 sec 6: "a unit mismatch
//! is a parse-time error, not a runtime surprise." [`eval::evaluate`] then computes the actual
//! number against [`runproducts::ExprRunProducts`] (trajectories, events, outputs --
//! "References resolve against run products"). [`objective::evaluate_objective`]/`evaluate_moe`
//! run all three steps, in that order, for one `Objective`/`MeasureOfEffectiveness`.
//!
//! ## `ExprRunProducts` vs. `crate::drm::executor::RunProducts` (question 93)
//!
//! Deliberately two different, differently-named types, not one type doing both jobs.
//! [`crate::drm::executor::RunProducts`] is `execute()`'s own *owned* return value -- everything
//! one DRM run produced: trajectories, events, evaluated `scores`, and the run's overall
//! `Provenance` (question 93). [`runproducts::ExprRunProducts`] is this module's *borrowed*,
//! read-only view built FROM that type's `trajectories`/`events` fields, plus an `outputs` map
//! `execute()` itself populates with one derived producer today (`runproducts::speed_output` --
//! see that module's own doc comment) -- it exists only to give the parser/typecheck/eval
//! pipeline something to resolve `ref`/`call` productions against, and a caller never keeps one
//! around after scoring. Before this task
//! `runproducts::RunProducts` and a hypothetical `execute()` return type would have collided on
//! the same name for two different things; renaming the evaluator's own type to
//! `ExprRunProducts` (rather than renaming `execute()`'s new return type, which is the one
//! question 93 actually names `RunProducts`) keeps `crate::drm::executor::RunProducts` as the
//! literal name the lead's decision uses.
//!
//! ## The amended grammar (ADR-005's original section 6 EBNF, corrected 2026-09-02)
//!
//! The lead's amendment fixed two defects the original EBNF had (found by the previous team
//! implementing it "as written", per the ADR's own amendment note): `call` had no `['@' time]`
//! suffix (so `range(a, b)@time`, the section's own reference form, was unparseable), and there
//! was no comparison-operator production at all (so `duration(range(a, b) < 100 m)`, the
//! section's own worked example, could not even be lexed). This crate now implements the
//! *corrected* grammar -- see [`parser`]'s module doc comment for the exact EBNF and
//! [`ast::Expr::At`]/[`ast::Expr::Compare`] for the two new AST shapes; `range`/`duration`'s
//! numerical semantics (not specified any more precisely by the ADR's own prose than its other
//! aggregates are) are disclosed in [`eval`]'s module doc comment, the same way that module
//! already discloses `min`/`max`/`mean`/`final`/`integral`'s.
//!
//! [`units`] similarly discloses (in its own doc comment) that the EBNF's `unit := one of the
//! CDM Unit names (m, m/s, rad, s, kg, ...)` names examples, not a complete token vocabulary or
//! a multiplication/division table -- both filled in there as an implementation detail within
//! the CDM `Unit` enum's own fixed vocabulary, disclosed rather than silently assumed.

pub mod ast;
pub mod error;
pub mod eval;
pub mod lexer;
pub mod objective;
pub mod parser;
pub mod runproducts;
pub mod typecheck;
pub mod units;

pub use ast::{BinOp, CmpOp, Expr, TimeSpec};
pub use error::ExprError;
pub use eval::{evaluate, Value};
pub use objective::{evaluate_moe, evaluate_objective, MoeResult, ObjectiveResult};
pub use parser::parse;
pub use runproducts::{speed_output, ExprRunProducts, Series, SPEED_OUTPUT_NAME};
pub use typecheck::check;
