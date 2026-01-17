//! Decimal arithmetic utilities for financial calculations.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Round a decimal to a specific number of decimal places.
pub fn round_to_precision(value: Decimal, decimals: u32) -> Decimal {
    value.round_dp(decimals)
}

/// Round to tick size (e.g., 0.01 for most prices).
pub fn round_to_tick(value: Decimal, tick_size: Decimal) -> Decimal {
    if tick_size == Decimal::ZERO {
        return value;
    }
    (value / tick_size).round() * tick_size
}

/// Round down to lot size (quantity precision).
pub fn round_down_to_lot(value: Decimal, lot_size: Decimal) -> Decimal {
    if lot_size == Decimal::ZERO {
        return value;
    }
    (value / lot_size).floor() * lot_size
}

/// Calculate percentage difference between two values.
pub fn percentage_diff(a: Decimal, b: Decimal) -> Decimal {
    if b == Decimal::ZERO {
        return Decimal::ZERO;
    }
    ((a - b) / b).abs() * dec!(100)
}

/// Calculate basis points (1 bp = 0.01%)
pub fn to_basis_points(rate: Decimal) -> Decimal {
    rate * dec!(10000)
}

/// Convert basis points to decimal rate
pub fn from_basis_points(bps: Decimal) -> Decimal {
    bps / dec!(10000)
}

/// Safe division that returns zero if divisor is zero.
pub fn safe_div(numerator: Decimal, denominator: Decimal) -> Decimal {
    if denominator == Decimal::ZERO {
        Decimal::ZERO
    } else {
        numerator / denominator
    }
}

/// Calculate weighted average.
pub fn weighted_average(values: &[(Decimal, Decimal)]) -> Decimal {
    let (sum, weight_sum) = values.iter().fold(
        (Decimal::ZERO, Decimal::ZERO),
        |(sum, weight_sum), (val, weight)| (sum + val * weight, weight_sum + weight),
    );

    safe_div(sum, weight_sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_to_tick() {
        assert_eq!(round_to_tick(dec!(50123.456), dec!(0.01)), dec!(50123.46));
        assert_eq!(round_to_tick(dec!(50123.456), dec!(0.10)), dec!(50123.50));
        assert_eq!(round_to_tick(dec!(50123.456), dec!(1.00)), dec!(50123.00));
    }

    #[test]
    fn test_round_down_to_lot() {
        assert_eq!(round_down_to_lot(dec!(1.567), dec!(0.001)), dec!(1.567));
        assert_eq!(round_down_to_lot(dec!(1.567), dec!(0.01)), dec!(1.56));
        assert_eq!(round_down_to_lot(dec!(1.567), dec!(0.1)), dec!(1.5));
    }

    #[test]
    fn test_basis_points() {
        assert_eq!(to_basis_points(dec!(0.0001)), dec!(1)); // 0.01% = 1 bp
        assert_eq!(to_basis_points(dec!(0.01)), dec!(100)); // 1% = 100 bp
        assert_eq!(from_basis_points(dec!(50)), dec!(0.005)); // 50 bp = 0.5%
    }

    #[test]
    fn test_weighted_average() {
        let values = vec![
            (dec!(100), dec!(2)), // 100 with weight 2
            (dec!(200), dec!(1)), // 200 with weight 1
        ];
        // (100*2 + 200*1) / (2+1) = 400/3 ≈ 133.33
        let avg = weighted_average(&values);
        assert!(avg > dec!(133) && avg < dec!(134));
    }
}
