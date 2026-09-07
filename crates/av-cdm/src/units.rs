//! SI <-> kilometre conversions, the one boundary crossing GMAT needs (ADR-001 "Units").
//!
//! `altavista.v1` is SI internally (metres, metres per second); kilometres exist only
//! inside the GMAT adapter (GMAT's propagators and `CoordinateSystem` work in km / km/s).
//! Every GMAT boundary crossing should go through this module rather than multiplying by
//! 1000 at the call site, so a unit bug has exactly one place to hide, not many.

/// Metres per kilometre.
pub const M_PER_KM: f64 = 1000.0;

pub fn km_to_m(km: f64) -> f64 {
    km * M_PER_KM
}

pub fn m_to_km(m: f64) -> f64 {
    m / M_PER_KM
}

pub fn kmps_to_mps(km_per_s: f64) -> f64 {
    km_per_s * M_PER_KM
}

pub fn mps_to_kmps(m_per_s: f64) -> f64 {
    m_per_s / M_PER_KM
}

/// A 6-element Cartesian state `[pos_x, pos_y, pos_z, vel_x, vel_y, vel_z]`, position in km
/// and velocity in km/s, converted to metres and metres/second.
pub fn state_km_to_m(state: [f64; 6]) -> [f64; 6] {
    let mut out = state;
    for v in &mut out[0..3] {
        *v = km_to_m(*v);
    }
    for v in &mut out[3..6] {
        *v = kmps_to_mps(*v);
    }
    out
}

/// A 6-element Cartesian state `[pos_x, pos_y, pos_z, vel_x, vel_y, vel_z]`, position in m
/// and velocity in m/s, converted to kilometres and kilometres/second.
pub fn state_m_to_km(state: [f64; 6]) -> [f64; 6] {
    let mut out = state;
    for v in &mut out[0..3] {
        *v = m_to_km(*v);
    }
    for v in &mut out[3..6] {
        *v = mps_to_kmps(*v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_conversions_are_exact_for_round_numbers() {
        assert_eq!(km_to_m(1.0), 1000.0);
        assert_eq!(m_to_km(1000.0), 1.0);
        assert_eq!(kmps_to_mps(7.5), 7500.0);
        assert_eq!(mps_to_kmps(7500.0), 7.5);
    }

    #[test]
    fn scalar_round_trips() {
        for km in [0.0, 1.0, -6378.137, 42164.0, 1.0e-6] {
            assert!((m_to_km(km_to_m(km)) - km).abs() < 1e-9);
        }
    }

    #[test]
    fn state_conversion_scales_position_and_velocity_independently() {
        let state_km = [7000.0, 0.0, 0.0, 0.0, 7.5, 0.0];
        let state_m = state_km_to_m(state_km);
        assert_eq!(state_m, [7_000_000.0, 0.0, 0.0, 0.0, 7500.0, 0.0]);
        assert_eq!(state_m_to_km(state_m), state_km);
    }

    #[test]
    fn state_round_trips() {
        let state_km = [7000.1, -300.25, 6.0, 1.234, -7.5, 0.001];
        let round = state_m_to_km(state_km_to_m(state_km));
        for (a, b) in state_km.iter().zip(round.iter()) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
    }
}
