//! Unit tokens (the EBNF's `unit := one of the CDM Unit names (m, m/s, rad, s, kg, ...)`) and
//! unit **composition** for `*`/`/` (ADR-005 sec 6: "Units propagate through arithmetic and a
//! unit mismatch is a parse-time error, not a runtime surprise").
//!
//! ## What ADR-005 sec 6 does and does not specify here
//!
//! The ADR gives the *rule* ("propagate through arithmetic") and, for `unit` itself, a few
//! example short spellings ("m, m/s, rad, s, kg, ..."), but no complete token vocabulary and
//! no multiplication/division table. Both are filled in below as an implementation detail
//! within the CDM `Unit` enum's own fixed vocabulary (`proto/altavista/v1/core.proto`) --
//! this is exactly the kind of gap the task's honesty rules ask to be disclosed, not resolved
//! silently, so it is spelled out here and again in the crate's top-level report.
//!
//! **Token vocabulary** (one canonical short spelling per non-`UNSPECIFIED` CDM `Unit`,
//! chosen from the ADR's own examples plus the obvious SI abbreviation for the rest):
//! dimensionless numbers carry no unit suffix at all; `m`, `m/s`, `m/s^2`, `rad`, `rad/s`,
//! `s`, `kg`, `kg/s`, `N`, `N*m`, `W`, `J`, `K`, `Pa`, `V`, `A`, `Hz`, `dB`.
//!
//! **Composition table** (`compose`): the CDM `Unit` enum has no generic per-dimension
//! exponent representation (no "m^2", no "1/m"), so a fully general dimensional-analysis
//! multiply/divide (the way a unit library over SI base dimensions would do it) cannot be
//! built from it. What *can* be built, without inventing a unit the enum does not declare, is
//! the closed set of products/quotients that land back on another **declared** `Unit` member,
//! using exactly the SI relationships the enum's own member names already imply (`UNIT_NEWTON`
//! is `kg*m/s^2`, `UNIT_NEWTON_METER` is `N*m`, `UNIT_WATT` is `J/s` = `V*A`, `UNIT_HERTZ` is
//! `1/s`, and so on). Anything outside that table (`m * m`, which would need an area unit the
//! CDM does not have, being the running example) is
//! [`crate::expr::error::ExprError::UnitCompositionUndefined`] -- refused, never silently
//! dropped to dimensionless or invented as a new unit. Multiplying or dividing by a
//! dimensionless operand is always defined (plain scaling) and needs no table entry.

use av_cdm::pb::Unit;

/// The one canonical short spelling `crate::expr::lexer` recognizes for each declared CDM
/// `Unit` (longest tokens first -- the lexer matches greedily and stops at the first
/// whole-token match with a word boundary after it, so `m/s^2` must be tried before `m/s`
/// before `m`, etc.). `UNIT_UNSPECIFIED` has no token: a bare number with no suffix is
/// [`Unit::Dimensionless`].
pub const UNIT_TOKENS: &[(&str, Unit)] = &[
    ("m/s^2", Unit::MeterPerSecondSquared),
    ("rad/s", Unit::RadianPerSecond),
    ("kg/s", Unit::KilogramPerSecond),
    ("m/s", Unit::MeterPerSecond),
    ("N*m", Unit::NewtonMeter),
    ("m", Unit::Meter),
    ("rad", Unit::Radian),
    ("s", Unit::Second),
    ("kg", Unit::Kilogram),
    ("N", Unit::Newton),
    ("W", Unit::Watt),
    ("J", Unit::Joule),
    ("K", Unit::Kelvin),
    ("Pa", Unit::Pascal),
    ("V", Unit::Volt),
    ("A", Unit::Ampere),
    ("Hz", Unit::Hertz),
    ("dB", Unit::Decibel),
];

/// Parse a raw unit-suffix token (as scanned by the lexer, e.g. `"m/s"`) into its CDM `Unit`.
/// `None` for a token the lexer should never actually produce (it only ever scans one of
/// [`UNIT_TOKENS`]'s left-hand sides) -- kept fallible anyway so a future lexer bug is a typed
/// `None` some caller must handle, not a panic.
pub fn parse_unit_token(tok: &str) -> Option<Unit> {
    UNIT_TOKENS.iter().find(|(t, _)| *t == tok).map(|(_, u)| *u)
}

/// The display form of `unit` for error messages: its canonical token (`"m/s"`), or
/// `"dimensionless"` / `"UNSPECIFIED"` for the two units with no token.
pub fn unit_display(unit: Unit) -> String {
    if unit == Unit::Dimensionless {
        return "dimensionless".to_string();
    }
    if unit == Unit::Unspecified {
        return "UNSPECIFIED".to_string();
    }
    UNIT_TOKENS.iter().find(|(_, u)| *u == unit).map(|(t, _)| t.to_string()).unwrap_or_else(|| format!("{unit:?}"))
}

/// `left op right` where `op` is `+` or `-`: identical units only, result is that unit.
/// `pos` is the byte offset of the operator, for the error.
pub fn add_sub_unit(op: &'static str, left: Unit, right: Unit, pos: usize) -> Result<Unit, crate::expr::error::ExprError> {
    if left == right {
        Ok(left)
    } else {
        Err(crate::expr::error::ExprError::UnitMismatch { op, left, right, pos })
    }
}

/// The declared multiplication table, as `(left, right) -> product`, `left`/`right` both
/// non-`Dimensionless` (dimensionless scaling is handled separately in [`compose`] and needs
/// no table entry). Every pair here is the direct SI relationship a CDM `Unit` member's own
/// name already implies; entered symmetrically (`(a, b)` and `(b, a)` both map to the same
/// product) since multiplication commutes. See the module doc comment for why this table
/// exists and is not a general dimensional-analysis engine.
fn mul_table(left: Unit, right: Unit) -> Option<Unit> {
    use Unit::*;
    let pair = (left, right);
    Some(match pair {
        // kg * m/s^2 = N (F = ma).
        (Kilogram, MeterPerSecondSquared) | (MeterPerSecondSquared, Kilogram) => Newton,
        // N * m = N*m (torque / work).
        (Newton, Meter) | (Meter, Newton) => NewtonMeter,
        // V * A = W (P = VI).
        (Volt, Ampere) | (Ampere, Volt) => Watt,
        // W * s = J (energy = power * time).
        (Watt, Second) | (Second, Watt) => Joule,
        // m/s * s = m (distance = velocity * time).
        (MeterPerSecond, Second) | (Second, MeterPerSecond) => Meter,
        // m/s^2 * s = m/s (velocity = accel * time).
        (MeterPerSecondSquared, Second) | (Second, MeterPerSecondSquared) => MeterPerSecond,
        // rad/s * s = rad.
        (RadianPerSecond, Second) | (Second, RadianPerSecond) => Radian,
        // kg/s * s = kg.
        (KilogramPerSecond, Second) | (Second, KilogramPerSecond) => Kilogram,
        // Hz * s = dimensionless (1/s * s = 1).
        (Hertz, Second) | (Second, Hertz) => Dimensionless,
        _ => return None,
    })
}

/// `left op right` where `op` is `*` or `/`: dimensionless scales freely; otherwise looked up
/// in [`mul_table`] (division is multiplication by the table's inverse relationship). `pos` is
/// the byte offset of the operator.
pub fn mul_div_unit(op: &'static str, left: Unit, right: Unit, pos: usize) -> Result<Unit, crate::expr::error::ExprError> {
    use Unit::Dimensionless;
    if op == "*" {
        if left == Dimensionless {
            return Ok(right);
        }
        if right == Dimensionless {
            return Ok(left);
        }
        return mul_table(left, right).ok_or(crate::expr::error::ExprError::UnitCompositionUndefined { op, left, right, pos });
    }
    // op == "/"
    if right == Dimensionless {
        return Ok(left);
    }
    if left == right {
        // X / X = dimensionless for any declared unit (including two dimensionless operands,
        // already handled above).
        return Ok(Dimensionless);
    }
    // a / b is defined exactly when some c with mul_table(b, c) == a (or (c, b) == a) exists;
    // that c is the quotient. Dimensionless is included as a candidate result (e.g. Hz / Hz
    // never reaches here since left==right is already handled; s / (1/Hz)-shaped cases are not
    // representable since Dimensionless/Second already goes through the Hertz-producing branch
    // in mul_table, not here).
    if left == Dimensionless {
        if let Some(q) = mul_table(right, Unit::Hertz) {
            // right * Hz landed on Dimensionless only for right == Second; that means
            // Dimensionless / Second == Hertz, the one dimensionless-numerator case this table
            // supports.
            if q == Dimensionless && right == Unit::Second {
                return Ok(Unit::Hertz);
            }
        }
        return Err(crate::expr::error::ExprError::UnitCompositionUndefined { op, left, right, pos });
    }
    for candidate in candidates() {
        if mul_table(right, candidate) == Some(left) || mul_table(candidate, right) == Some(left) {
            return Ok(candidate);
        }
    }
    Err(crate::expr::error::ExprError::UnitCompositionUndefined { op, left, right, pos })
}

/// Every unit [`mul_table`] can ever produce or accept, for [`mul_div_unit`]'s division search.
fn candidates() -> [Unit; 20] {
    [
        Unit::Unspecified,
        Unit::Dimensionless,
        Unit::Meter,
        Unit::MeterPerSecond,
        Unit::MeterPerSecondSquared,
        Unit::Radian,
        Unit::RadianPerSecond,
        Unit::Second,
        Unit::Kilogram,
        Unit::KilogramPerSecond,
        Unit::Newton,
        Unit::NewtonMeter,
        Unit::Watt,
        Unit::Joule,
        Unit::Kelvin,
        Unit::Pascal,
        Unit::Volt,
        Unit::Ampere,
        Unit::Hertz,
        Unit::Decibel,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_documented_example_token() {
        assert_eq!(parse_unit_token("m"), Some(Unit::Meter));
        assert_eq!(parse_unit_token("m/s"), Some(Unit::MeterPerSecond));
        assert_eq!(parse_unit_token("rad"), Some(Unit::Radian));
        assert_eq!(parse_unit_token("s"), Some(Unit::Second));
        assert_eq!(parse_unit_token("kg"), Some(Unit::Kilogram));
        assert_eq!(parse_unit_token("bogus"), None);
    }

    #[test]
    fn add_sub_requires_identical_units() {
        assert_eq!(add_sub_unit("+", Unit::Meter, Unit::Meter, 0), Ok(Unit::Meter));
        let err = add_sub_unit("+", Unit::Meter, Unit::Second, 5).unwrap_err();
        assert!(matches!(err, crate::expr::error::ExprError::UnitMismatch { op: "+", left: Unit::Meter, right: Unit::Second, pos: 5 }));
    }

    #[test]
    fn mul_div_scales_freely_by_dimensionless() {
        assert_eq!(mul_div_unit("*", Unit::Meter, Unit::Dimensionless, 0), Ok(Unit::Meter));
        assert_eq!(mul_div_unit("/", Unit::Meter, Unit::Dimensionless, 0), Ok(Unit::Meter));
        assert_eq!(mul_div_unit("*", Unit::Dimensionless, Unit::Meter, 0), Ok(Unit::Meter));
    }

    #[test]
    fn mul_div_follows_declared_si_relationships() {
        assert_eq!(mul_div_unit("*", Unit::Kilogram, Unit::MeterPerSecondSquared, 0), Ok(Unit::Newton));
        assert_eq!(mul_div_unit("/", Unit::Newton, Unit::Kilogram, 0), Ok(Unit::MeterPerSecondSquared));
        assert_eq!(mul_div_unit("/", Unit::Newton, Unit::MeterPerSecondSquared, 0), Ok(Unit::Kilogram));
        assert_eq!(mul_div_unit("*", Unit::MeterPerSecond, Unit::Second, 0), Ok(Unit::Meter));
        assert_eq!(mul_div_unit("/", Unit::Meter, Unit::Second, 0), Ok(Unit::MeterPerSecond));
        assert_eq!(mul_div_unit("/", Unit::Meter, Unit::MeterPerSecond, 0), Ok(Unit::Second));
        assert_eq!(mul_div_unit("*", Unit::Volt, Unit::Ampere, 0), Ok(Unit::Watt));
        assert_eq!(mul_div_unit("/", Unit::Watt, Unit::Ampere, 0), Ok(Unit::Volt));
        assert_eq!(mul_div_unit("/", Unit::Dimensionless, Unit::Second, 0), Ok(Unit::Hertz));
    }

    #[test]
    fn mul_div_refuses_an_undeclared_composition_rather_than_invent_a_unit() {
        // m * m would need an area unit the CDM Unit enum does not declare.
        let err = mul_div_unit("*", Unit::Meter, Unit::Meter, 3).unwrap_err();
        assert!(matches!(err, crate::expr::error::ExprError::UnitCompositionUndefined { op: "*", left: Unit::Meter, right: Unit::Meter, pos: 3 }));
    }

    #[test]
    fn div_by_self_is_dimensionless() {
        assert_eq!(mul_div_unit("/", Unit::Meter, Unit::Meter, 0), Ok(Unit::Dimensionless));
        assert_eq!(mul_div_unit("/", Unit::Newton, Unit::Newton, 0), Ok(Unit::Dimensionless));
    }
}
