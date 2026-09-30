//! SunSpec scale factors (`sunssf`). SunSpec models store a value as an
//! integer and its decimal exponent in a separate int16 register (model
//! 103's W at 40084, W_SF at 40085): value = raw × 10^sf.

/// SunSpec's "not implemented" marker for an int16 register.
const NOT_IMPLEMENTED: u16 = 0x8000;

/// Apply a scale factor given as its raw register word: `raw × 10^sf`.
/// "Not implemented" (`0x8000`) is an error, not a scale.
pub fn apply_sunssf(raw: f64, sf_word: u16) -> Result<f64, String> {
    if sf_word == NOT_IMPLEMENTED {
        return Err("scale factor register reads 0x8000 (not implemented)".to_string());
    }
    // Reason: the register is a two's-complement int16; -1 arrives as 0xFFFF.
    let sf = sf_word as i16;
    Ok(raw * 10f64.powi(i32::from(sf)))
}
