//! Pure formula evaluator for synthetic channels.
//!
//! 5-value formula vocabulary locked in handoff Q-C: subtract, sum, mean,
//! max, min. Adding a new formula = new enum variant + new arm in `apply` +
//! new test case. No string-eval, no expression parser — keep it boring.

use anyhow::{Result, anyhow};

/// One named pure function applied to N cached float inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Formula {
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

impl Formula {
    /// Parse a formula name (matches the JSON `formula` field). Unknown names
    /// = error, surfaces at gateway startup not runtime.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "subtract" => Ok(Self::Subtract),
            "sum" => Ok(Self::Sum),
            "mean" => Ok(Self::Mean),
            "max" => Ok(Self::Max),
            "min" => Ok(Self::Min),
            other => Err(anyhow!("unknown synthetic formula: {other}")),
        }
    }

    /// Apply the formula to the cached input values. Returns an error when
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

/// Helper: error out when an aggregate formula (sum/mean/max/min) gets zero inputs.
fn require_nonempty(inputs: &[f64], name: &str) -> Result<()> {
    if inputs.is_empty() {
        return Err(anyhow!("{name} requires at least one input"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_recognizes_all_five_formulas() {
        for (name, expected) in [
            ("subtract", Formula::Subtract),
            ("sum", Formula::Sum),
            ("mean", Formula::Mean),
            ("max", Formula::Max),
            ("min", Formula::Min),
        ] {
            assert_eq!(Formula::parse(name).unwrap(), expected);
        }
    }

    #[test]
    fn parse_rejects_unknown_formula() {
        assert!(Formula::parse("divide").is_err());
    }

    #[test]
    fn subtract_returns_a_minus_b() {
        // Arrange — DOE import_limit (10MW) minus active_power (3MW) = 7MW headroom
        let inputs = [10_000_000.0, 3_000_000.0];
        // Act
        let got = Formula::Subtract.apply(&inputs).unwrap();
        // Assert
        assert!((got - 7_000_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn subtract_rejects_wrong_arity() {
        assert!(Formula::Subtract.apply(&[1.0]).is_err());
        assert!(Formula::Subtract.apply(&[1.0, 2.0, 3.0]).is_err());
    }

    #[test]
    fn sum_mean_max_min_compute_correctly() {
        let inputs = [1.0, 2.0, 3.0, 4.0];
        assert!((Formula::Sum.apply(&inputs).unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((Formula::Mean.apply(&inputs).unwrap() - 2.5).abs() < f64::EPSILON);
        assert!((Formula::Max.apply(&inputs).unwrap() - 4.0).abs() < f64::EPSILON);
        assert!((Formula::Min.apply(&inputs).unwrap() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_inputs_rejected_for_aggregate_formulas() {
        for f in [Formula::Sum, Formula::Mean, Formula::Max, Formula::Min] {
            assert!(f.apply(&[]).is_err());
        }
    }
}
