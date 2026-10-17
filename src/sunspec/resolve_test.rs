use crate::asyncapi::types::SunspecBinding;
use crate::sunspec::discover::ModelMap;
use crate::sunspec::resolve::{address, readable, writable};

fn binding(model_id: u16, point: &str, bit: Option<u8>) -> SunspecBinding {
    SunspecBinding {
        host: "pcs".to_string(),
        port: 502,
        unit_id: "1".to_string(),
        model_id,
        point: point.to_string(),
        bit,
    }
}

#[test]
fn a_shipped_point_reads() {
    assert_eq!(readable(&binding(103, "W", None)).unwrap().offset, 12);
}

#[test]
fn an_unshipped_model_or_point_is_refused() {
    assert!(readable(&binding(701, "W", None)).is_err());
    assert!(readable(&binding(103, "Watts", None)).is_err());
}

#[test]
fn a_bit_is_only_one_of_a_bitfield_s_32() {
    assert!(readable(&binding(65534, "CmdBits", Some(1))).is_ok());
    assert!(readable(&binding(65534, "CmdBits", Some(32))).is_err());
    assert!(readable(&binding(103, "W", Some(0))).is_err());
}

#[test]
fn only_writable_points_take_commands() {
    assert!(writable(&binding(65534, "CmdRealPwr", None)).is_ok());
    assert!(writable(&binding(103, "W", None)).is_err());
}

#[test]
fn a_bitfield_is_written_one_named_bit_at_a_time() {
    assert!(writable(&binding(65534, "CmdBits", Some(0))).is_ok());
    assert!(writable(&binding(65534, "CmdBits", None)).is_err());
}

#[test]
fn a_point_and_its_scale_factor_sit_past_their_model_s_start() {
    // Arrange — model 103's first point at 40072, as on a stock unit
    let b = binding(103, "W", None);
    let models = ModelMap::from([(103, 40072)]);
    // Act
    let actual = address(&models, &b, readable(&b).unwrap()).unwrap();
    // Assert
    assert_eq!(actual, (40084, Some(40085)));
}
