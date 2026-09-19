//! Pure operation evaluator for synthetic channels.
//!
//! 5-value operation vocabulary locked in handoff Q-C: subtract, sum, mean,
//! max, min. Adding a new operation = new enum variant + new arm in `apply` +
//! new test case. No string-eval, no expression parser — keep it boring.

use anyhow::{Result, anyhow};

/// One named pure function applied to N cached float inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// `inputs[0] - inputs[1]`. Exactly two inputs.
    Subtract,
    /// Sum of all inputs. One or more.
    Sum,
    /// Arithmetic mean. One or more.
    Mean,
    /// Largest input. One or more.
    Max,
    /// Smallest input. One or more.
    Min,
}

impl Operation {
    /// Parse an operation name (matches the JSON `operation` field). Unknown
    /// names = error, surfaces at gateway startup not runtime.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "subtract" => Ok(Self::Subtract),
            "sum" => Ok(Self::Sum),
            "mean" => Ok(Self::Mean),
            "max" => Ok(Self::Max),
            "min" => Ok(Self::Min),
            other => Err(anyhow!("unknown synthetic operation: {other}")),
        }
    }

    /// Apply the operation to the cached input values. Returns an error when
    /// arity is wrong (e.g., subtract with !=2 inputs).
    pub fn apply(self, inputs: &[f64]) -> Result<f64> {
        match self {
            Self::Subtract => {
                if inputs.len() != 2 {
                    return Err(anyhow!(
                        "subtract requires exactly 2 inputs, got {}",
                        inputs.len()
                    ));
                }
                Ok(inputs[0] - inputs[1])
            }
            Self::Sum => {
                require_nonempty(inputs, "sum")?;
                Ok(inputs.iter().sum())
            }
            Self::Mean => {
                require_nonempty(inputs, "mean")?;
                #[allow(clippy::cast_precision_loss)]
                let len = inputs.len() as f64;
                Ok(inputs.iter().sum::<f64>() / len)
            }
            Self::Max => {
                require_nonempty(inputs, "max")?;
                Ok(inputs.iter().copied().fold(f64::NEG_INFINITY, f64::max))
            }
            Self::Min => {
                require_nonempty(inputs, "min")?;
                Ok(inputs.iter().copied().fold(f64::INFINITY, f64::min))
            }
        }
    }
}

/// Helper: error out when an aggregate operation (sum/mean/max/min) gets zero inputs.
fn require_nonempty(inputs: &[f64], name: &str) -> Result<()> {
    if inputs.is_empty() {
        return Err(anyhow!("{name} requires at least one input"));
    }
    Ok(())
}

/// Capacity-weighted mean of `(value, weight)` pairs — e.g. per-rack
/// `state_of_charge` weighted by `capacity_kwh`, so a rack at 2x a
/// sibling's capacity counts 2x as much. Not an `Operation` variant: every
/// `Operation::apply` arm takes flat `&[f64]`, and this needs pairs — a
/// separate function keeps that signature boring instead of bending it.
pub fn weighted_mean(pairs: &[(f64, f64)]) -> Result<f64> {
    if pairs.is_empty() {
        return Err(anyhow!("weighted_mean requires at least one pair"));
    }
    let total_weight: f64 = pairs.iter().map(|(_, w)| w).sum();
    if total_weight <= 0.0 {
        return Err(anyhow!(
            "weighted_mean requires a positive total weight, got {total_weight}"
        ));
    }
    let weighted_sum: f64 = pairs.iter().map(|(v, w)| v * w).sum();
    Ok(weighted_sum / total_weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_all_five_operations() {
        for (name, expected) in [
            ("subtract", Operation::Subtract),
            ("sum", Operation::Sum),
            ("mean", Operation::Mean),
            ("max", Operation::Max),
            ("min", Operation::Min),
        ] {
            assert_eq!(Operation::parse(name).unwrap(), expected);
        }
    }

    #[test]
    fn parse_rejects_unknown_operation() {
        assert!(Operation::parse("divide").is_err());
    }

    #[test]
    fn subtract_returns_a_minus_b() {
        // Arrange — DOE import_limit (10MW) minus active_power (3MW) = 7MW headroom
        let inputs = [10_000_000.0, 3_000_000.0];
        // Act
        let got = Operation::Subtract.apply(&inputs).unwrap();
        // Assert
        assert!((got - 7_000_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn subtract_rejects_wrong_arity() {
        assert!(Operation::Subtract.apply(&[1.0]).is_err());
        assert!(Operation::Subtract.apply(&[1.0, 2.0, 3.0]).is_err());
    }

    #[test]
    fn sum_mean_max_min_compute_correctly() {
        let inputs = [1.0, 2.0, 3.0, 4.0];
        assert!((Operation::Sum.apply(&inputs).unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((Operation::Mean.apply(&inputs).unwrap() - 2.5).abs() < f64::EPSILON);
        assert!((Operation::Max.apply(&inputs).unwrap() - 4.0).abs() < f64::EPSILON);
        assert!((Operation::Min.apply(&inputs).unwrap() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_inputs_rejected_for_aggregate_operations() {
        for f in [
            Operation::Sum,
            Operation::Mean,
            Operation::Max,
            Operation::Min,
        ] {
            assert!(f.apply(&[]).is_err());
        }
    }

    #[test]
    fn weighted_mean_weights_by_capacity() {
        // Arrange — two racks: 50% SoC at 2 MWh, 80% SoC at 1 MWh
        let pairs = [(50.0, 2.0), (80.0, 1.0)];
        // Act
        let got = weighted_mean(&pairs).unwrap();
        // Assert — (50*2 + 80*1) / (2+1) = 60.0, not the flat mean of 65.0
        assert!((got - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_mean_with_equal_weights_matches_flat_mean() {
        let pairs = [(10.0, 1.0), (20.0, 1.0)];
        let got = weighted_mean(&pairs).unwrap();
        assert!((got - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn weighted_mean_rejects_empty_pairs() {
        assert!(weighted_mean(&[]).is_err());
    }

    #[test]
    fn weighted_mean_rejects_zero_total_weight() {
        assert!(weighted_mean(&[(50.0, 0.0), (80.0, 0.0)]).is_err());
    }
}
