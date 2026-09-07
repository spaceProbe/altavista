//! Every way an ADR-005 sec 6 expression can be refused, typed rather than a caller ever
//! discovering a silently-coerced unit or a silently-ignored reference (the same "typed
//! refusal, never a silent guess" convention `crate::drm::DrmError` and
//! `crate::interpolate::InterpolationError` already use in this crate).
//!
//! Every variant that can be attributed to a place in the source expression carries `pos`: a
//! zero-based **byte offset** into the original expression string (not a line/column -- these
//! expressions are always one line). `ExprError::Display` always names both operand units for
//! a unit problem (the "typed error naming both units and the expression position" the task
//! requires) and never just says "unit mismatch".

use av_cdm::pb::Unit;
use std::fmt;

use crate::expr::units::unit_display;

#[derive(Debug, Clone, PartialEq)]
pub enum ExprError {
    // -- Lexing / parsing (ADR-005 sec 6's EBNF, implemented literally -- see
    // `crate::expr::parser`'s module doc comment for exactly which productions exist). --
    /// A character sequence the lexer does not recognize at all.
    Lex { pos: usize, message: String },
    /// A token appeared where the grammar's next production did not allow it.
    UnexpectedToken { pos: usize, found: String, expected: &'static str },
    /// The source ended before a production that was already committed to (an open `(`, a
    /// call's `)`, an operand after an operator) could finish.
    UnexpectedEof { expected: &'static str },
    /// Trailing input after a complete `expr` was parsed (e.g. `range(a, b)@end` -- see the
    /// crate's module doc comment: `call` has no `['@' time]` in the written grammar, so a
    /// `@` immediately after a call's `)` is unparseable trailing input, not a silently
    /// accepted extension).
    TrailingInput { pos: usize, found: String },

    // -- Function / reference name resolution (typecheck time: structural, no run data). --
    /// A `call` name is not one of this evaluator's recognized aggregate/count functions.
    UnknownFunction { name: String, pos: usize },
    /// A recognized function was called with the wrong number of arguments.
    WrongArity { name: String, expected: usize, got: usize, pos: usize },
    /// `min`/`max`/`mean`/`final`/`integral` require their one argument to be a bare
    /// reference with no explicit `@time` (ADR-005 sec 6 prose: `min(ref)`, not `min(expr)`)
    /// -- see this crate's module doc comment for why this is a deliberate narrowing of the
    /// literal `call := name '(' [expr...] ')'` production, not a silent extension of it.
    AggregateArgumentMustBeBareReference { name: String, pos: usize },
    /// A `ref` did not match any of the shapes this evaluator resolves
    /// (`entity.<id>.<component>`, `event.<name>.t`, `output.<instance>.<name>`), or carried a
    /// `@time` where one is not meaningful (`event.<name>.t` is already a single instant).
    UnknownReferenceShape { path: String, pos: usize },
    /// An `entity.<id>.<component>` or `output.<instance>.<name>` reference was evaluated
    /// directly (outside an aggregate) with no `@time` -- it denotes a whole time series, not
    /// a scalar, so ADR-005 sec 6's own reference forms always attach `@time` when a scalar is
    /// wanted; the aggregate functions (`min`/`max`/`mean`/`final`/`integral`) are the only
    /// place a bare series-valued reference is meaningful.
    BareReferenceOutsideAggregate { path: String, pos: usize },
    /// `entity.<id>...` named an id this run's products do not contain.
    UnknownEntity { id: String, pos: usize },
    /// `entity.<id>.<component>` named a component the id's declared `StateSpace` does not
    /// have a label for.
    UnknownComponent { entity: String, component: String, pos: usize },
    /// `output.<instance>.<name>` named an (instance, name) pair this run's products do not
    /// contain.
    UnknownOutput { instance: String, name: String, pos: usize },
    /// `event.<name>.t` named no event in this run's products (by `Event.name`).
    UnknownEventName { name: String, pos: usize },
    /// `event.<name>.t` (or the `@<ident>` time form) matched more than one event by name --
    /// refused rather than picking one arbitrarily.
    AmbiguousEventName { name: String, pos: usize },
    /// `count(event.<kind>)`'s `<kind>` did not match a declared `EventKind` (by its
    /// `EVENT_KIND_` name, lowercased with the prefix stripped -- see `crate::expr::eval`).
    UnknownEventKind { kind: String, pos: usize },
    /// A `range(<a>, <b>)` argument was not a bare, single-segment entity id (`range`'s two
    /// arguments name entities directly, e.g. `range(leo, gs)` -- not
    /// `entity.<id>.<component>`, which would be ambiguous about which component to use for a
    /// 3-vector distance).
    RangeArgumentMustBeEntityId { pos: usize },
    /// `duration(<condition>)`'s one argument must literally be a `comparison` node (ADR-005
    /// sec 6: "Aggregates over the run: ... `duration(condition)`", and the amendment's own
    /// worked example is `duration(range(a, b) < 100 m)`) -- refused rather than silently
    /// treating a non-comparison argument as "always true"/"always false".
    DurationArgumentMustBeComparison { pos: usize },
    /// `'@' time` was applied to a primary this evaluator has no time-indexed meaning for (only
    /// an `entity.<id>.<component>` reference, an `output.<instance>.<name>` reference, or a
    /// `range(<a>, <b>)` call can be read "at a time" -- a number, a parenthesized arithmetic
    /// expression, or any other call cannot).
    AtNotApplicable { pos: usize },

    // -- Units (ADR-005 sec 6: "a unit mismatch is a parse-time error, not a runtime
    // surprise"). Every variant names both operand units, per the task's honesty rule. --
    /// `+`/`-` requires identical units on both operands.
    UnitMismatch { op: &'static str, left: Unit, right: Unit, pos: usize },
    /// `*`/`/` produced a unit combination this evaluator's composition table does not
    /// declare (`crate::expr::units::compose`'s module doc comment lists exactly what is
    /// declared and why the CDM `Unit` enum cannot support a fully general one).
    UnitCompositionUndefined { op: &'static str, left: Unit, right: Unit, pos: usize },
    /// A time offset (`number 's'` in the `time` production) was written with a unit other
    /// than seconds, or with no unit at all.
    TimeMustBeSeconds { unit: Unit, pos: usize },
    /// An `Objective`/`MeasureOfEffectiveness.unit` was declared (non-`UNIT_UNSPECIFIED`) and
    /// disagreed with the expression's own propagated unit.
    DeclaredUnitMismatch { declared: Unit, computed: Unit },

    // -- Run-products plumbing. --
    /// The run's `ExprRunProducts` has no scenario window at all (`start_tai_ns >= end_tai_ns`),
    /// so `@start`/`@end` and every aggregate are meaningless.
    EmptyRun,
    /// `entity.<id>...` resolved the id, but that trajectory has zero samples -- a run that
    /// produced no output at all for a bound instance (not expected from this crate's own
    /// executor, but not assumed away either).
    EmptyTrajectory { id: String, pos: usize },
    /// A requested `@time` fell outside `[start_tai_ns, end_tai_ns]`.
    TimeOutOfRange { requested_s: f64, start_s: f64, end_s: f64, pos: usize },
    /// `crate::interpolate::interpolate_by_state_space` refused to interpolate the component
    /// (never silently guessed at -- see that module's own doc comment).
    Interpolation { entity: String, pos: usize, source: String },
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprError::Lex { pos, message } => write!(f, "position {pos}: {message}"),
            ExprError::UnexpectedToken { pos, found, expected } => write!(f, "position {pos}: unexpected {found}, expected {expected}"),
            ExprError::UnexpectedEof { expected } => write!(f, "unexpected end of expression, expected {expected}"),
            ExprError::TrailingInput { pos, found } => write!(f, "position {pos}: unexpected trailing input {found} after a complete expression"),
            ExprError::UnknownFunction { name, pos } => write!(f, "position {pos}: {name:?} is not a recognized function (min, max, mean, final, integral, count)"),
            ExprError::WrongArity { name, expected, got, pos } => write!(f, "position {pos}: {name}() takes {expected} argument(s), got {got}"),
            ExprError::AggregateArgumentMustBeBareReference { name, pos } => {
                write!(f, "position {pos}: {name}() requires a bare reference argument with no '@time' (ADR-005 sec 6: `{name}(ref)`)")
            }
            ExprError::UnknownReferenceShape { path, pos } => {
                write!(f, "position {pos}: {path:?} is not a recognized reference shape (entity.<id>.<component>, event.<name>.t, output.<instance>.<name>)")
            }
            ExprError::BareReferenceOutsideAggregate { path, pos } => {
                write!(f, "position {pos}: {path:?} has no '@time' and is not the argument of an aggregate function; it names a whole time series, not a scalar")
            }
            ExprError::UnknownEntity { id, pos } => write!(f, "position {pos}: no entity {id:?} in this run's products"),
            ExprError::UnknownComponent { entity, component, pos } => write!(f, "position {pos}: entity {entity:?} has no component {component:?} in its declared StateSpace"),
            ExprError::UnknownOutput { instance, name, pos } => write!(f, "position {pos}: no output {name:?} for instance {instance:?} in this run's products"),
            ExprError::UnknownEventName { name, pos } => write!(f, "position {pos}: no event named {name:?} in this run's products"),
            ExprError::AmbiguousEventName { name, pos } => write!(f, "position {pos}: more than one event is named {name:?}; refusing to pick one"),
            ExprError::UnknownEventKind { kind, pos } => write!(f, "position {pos}: {kind:?} is not a recognized EventKind"),
            ExprError::RangeArgumentMustBeEntityId { pos } => write!(f, "position {pos}: range()'s arguments must be bare entity ids (e.g. range(leo, gs)), not a dotted reference or expression"),
            ExprError::DurationArgumentMustBeComparison { pos } => write!(f, "position {pos}: duration() requires its one argument to be a comparison (e.g. duration(range(a, b) < 100 m))"),
            ExprError::AtNotApplicable { pos } => write!(f, "position {pos}: '@time' does not apply here (only an entity/output reference or range(...) can be read at a time)"),
            ExprError::UnitMismatch { op, left, right, pos } => {
                write!(f, "position {pos}: unit mismatch in '{op}': {} vs {}", unit_display(*left), unit_display(*right))
            }
            ExprError::UnitCompositionUndefined { op, left, right, pos } => {
                write!(f, "position {pos}: no declared CDM unit for {} '{op}' {}", unit_display(*left), unit_display(*right))
            }
            ExprError::TimeMustBeSeconds { unit, pos } => write!(f, "position {pos}: a time offset must be in seconds ('s'), got {}", unit_display(*unit)),
            ExprError::DeclaredUnitMismatch { declared, computed } => {
                write!(f, "declared unit {} does not match the expression's own propagated unit {}", unit_display(*declared), unit_display(*computed))
            }
            ExprError::EmptyRun => write!(f, "run has an empty scenario window (start_tai_ns >= end_tai_ns); @start/@end and aggregates are meaningless"),
            ExprError::EmptyTrajectory { id, pos } => write!(f, "position {pos}: entity {id:?} has zero samples in this run's products"),
            ExprError::TimeOutOfRange { requested_s, start_s, end_s, pos } => {
                write!(f, "position {pos}: time {requested_s} s is outside the run's window [{start_s}, {end_s}] s")
            }
            ExprError::Interpolation { entity, pos, source } => write!(f, "position {pos}: entity {entity:?}: {source}"),
        }
    }
}
impl std::error::Error for ExprError {}
