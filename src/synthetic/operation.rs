//! Pure operation evaluator for synthetic channels.
//!
//! Operations: subtract, sum, mean, max, min, unbalance. Adding one = new
//! enum variant + new arm in `apply` + new test case. No string-eval, no
//! expression parser — keep it boring.

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
    /// Voltage unbalance in percent: `100 × max|x − mean| / mean`
    /// (NEMA MG-1 style). One or more.
    Unbalance,
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
            "unbalance" => Ok(Self::Unbalance),
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
            Self::Unbalance => {
                let mean = Self::Mean.apply(inputs)?;
                // Reason: a dead bus (mean 0 V) has no unbalance; 0% would read as healthy
                if mean <= 0.0 {
                    return Err(anyhow!("unbalance needs a positive mean, got {mean}"));
                }
                let deviation = inputs.iter().map(|x| (x - mean).abs()).fold(0.0, f64::max);
                Ok(100.0 * deviation / mean)
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
#[path = "operation_test.rs"]
mod tests;
