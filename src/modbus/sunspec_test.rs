//! SunSpec scale factors. High-risk: a misapplied sunssf reads an inverter
//! off by powers of ten.

use super::sunspec::apply_sunssf;

#[test]
fn sunssf_scales_by_a_power_of_ten() {
    // SunSpec model 103: W = 1234 with W_SF = -1 is 123.4 W
    assert_eq!(apply_sunssf(1234.0, 0xFFFF), Ok(123.4));
    assert_eq!(apply_sunssf(12.0, 2), Ok(1200.0));
}

#[test]
fn an_unimplemented_sunssf_is_an_error_not_a_scale() {
    // 0x8000 is SunSpec's "not implemented"; 10^-32768 would read as 0
    assert!(apply_sunssf(1234.0, 0x8000).is_err());
}
