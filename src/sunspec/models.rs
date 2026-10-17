//! SunSpec model definitions the gateway ships: per model, each numeric
//! point's offset from the model's first register (just past its ID/L
//! header), its type, its scale-factor point and whether it's writable.
//! Strings and pads are left out; nothing binds them.
//!
//! Models 1 (common) and 103 (three-phase inverter) are standard SunSpec;
//! 65534 is EPC Power's `epc_control` (CAB1000 PCS, firmware ~1.2.x layout).

/// A SunSpec point type, as the model definitions spell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointType {
    /// Signed 16-bit.
    Int16,
    /// Unsigned 16-bit.
    Uint16,
    /// Signed 32-bit.
    Int32,
    /// Unsigned 32-bit accumulator.
    Acc32,
    /// Unsigned 16-bit code; labels come from the binding's value_map.
    Enum16,
    /// 32 flag bits.
    Bitfield32,
    /// int16 decimal exponent another point scales by.
    Sunssf,
}

impl PointType {
    /// Registers the point spans.
    pub fn register_count(self) -> u16 {
        match self {
            Self::Int16 | Self::Uint16 | Self::Enum16 | Self::Sunssf => 1,
            Self::Int32 | Self::Acc32 | Self::Bitfield32 => 2,
        }
    }
}

/// One point of a model.
#[derive(Debug, PartialEq, Eq)]
pub struct PointDef {
    /// Point id, e.g. `W`.
    pub id: &'static str,
    /// Registers past the model's first point.
    pub offset: u16,
    /// Wire type.
    pub kind: PointType,
    /// The sunssf point this one scales by, in the same model.
    pub sf: Option<&'static str>,
    /// Access `rw` in the model definition.
    pub writable: bool,
}

use PointType::{Acc32, Bitfield32, Enum16, Int16, Int32, Sunssf, Uint16};

/// Read-only point.
const fn r(id: &'static str, offset: u16, kind: PointType, sf: Option<&'static str>) -> PointDef {
    PointDef {
        id,
        offset,
        kind,
        sf,
        writable: false,
    }
}

/// Writable point.
const fn w(id: &'static str, offset: u16, kind: PointType, sf: Option<&'static str>) -> PointDef {
    PointDef {
        id,
        offset,
        kind,
        sf,
        writable: true,
    }
}

/// Model 1, common.
const COMMON: &[PointDef] = &[w("DA", 64, Uint16, None)];

/// Model 103, three-phase inverter.
const INVERTER_THREE_PHASE: &[PointDef] = &[
    r("A", 0, Uint16, Some("A_SF")),
    r("AphA", 1, Uint16, Some("A_SF")),
    r("AphB", 2, Uint16, Some("A_SF")),
    r("AphC", 3, Uint16, Some("A_SF")),
    r("A_SF", 4, Sunssf, None),
    r("PPVphAB", 5, Uint16, Some("V_SF")),
    r("PPVphBC", 6, Uint16, Some("V_SF")),
    r("PPVphCA", 7, Uint16, Some("V_SF")),
    r("PhVphA", 8, Uint16, Some("V_SF")),
    r("PhVphB", 9, Uint16, Some("V_SF")),
    r("PhVphC", 10, Uint16, Some("V_SF")),
    r("V_SF", 11, Sunssf, None),
    r("W", 12, Int16, Some("W_SF")),
    r("W_SF", 13, Sunssf, None),
    r("Hz", 14, Uint16, Some("Hz_SF")),
    r("Hz_SF", 15, Sunssf, None),
    r("VA", 16, Int16, Some("VA_SF")),
    r("VA_SF", 17, Sunssf, None),
    r("VAr", 18, Int16, Some("VAr_SF")),
    r("VAr_SF", 19, Sunssf, None),
    r("PF", 20, Int16, Some("PF_SF")),
    r("PF_SF", 21, Sunssf, None),
    r("WH", 22, Acc32, Some("WH_SF")),
    r("WH_SF", 24, Sunssf, None),
    r("DCA", 25, Uint16, Some("DCA_SF")),
    r("DCA_SF", 26, Sunssf, None),
    r("DCV", 27, Uint16, Some("DCV_SF")),
    r("DCV_SF", 28, Sunssf, None),
    r("DCW", 29, Int16, Some("DCW_SF")),
    r("DCW_SF", 30, Sunssf, None),
    r("TmpCab", 31, Int16, Some("Tmp_SF")),
    r("TmpSnk", 32, Int16, Some("Tmp_SF")),
    r("TmpTrns", 33, Int16, Some("Tmp_SF")),
    r("TmpOt", 34, Int16, Some("Tmp_SF")),
    r("Tmp_SF", 35, Sunssf, None),
    r("St", 36, Enum16, None),
    r("StVnd", 37, Enum16, None),
    r("Evt1", 38, Bitfield32, None),
    r("Evt2", 40, Bitfield32, None),
    r("EvtVnd1", 42, Bitfield32, None),
    r("EvtVnd2", 44, Bitfield32, None),
    r("EvtVnd3", 46, Bitfield32, None),
    r("EvtVnd4", 48, Bitfield32, None),
];

/// Model 65534, EPC `epc_control`.
const EPC_CONTROL: &[PointDef] = &[
    w("CmdBits", 0, Bitfield32, None),
    w("CmdRealPwr", 2, Int32, Some("PWR_SF")),
    w("CmdReactivePwr", 4, Int32, Some("PWR_SF")),
    r("PWR_SF", 6, Sunssf, None),
    w("CmdV", 7, Uint16, Some("V_SF")),
    r("V_SF", 8, Sunssf, None),
    w("CmdHz", 9, Uint16, Some("HZ_SF")),
    r("HZ_SF", 10, Sunssf, None),
    w("CtlSrc", 11, Enum16, None),
    r("IMPPT", 12, Int16, Some("A_SF")),
    r("Ivctl", 13, Int16, Some("A_SF")),
    r("Vsol", 14, Int16, Some("V_SF")),
    r("Vbat", 15, Int16, Some("V_SF")),
    r("A_SF", 16, Sunssf, None),
    w("Spcl", 17, Bitfield32, None),
    r("SysFlt", 19, Uint16, None),
    r("FltFlgs", 20, Bitfield32, None),
    r("WrnFlgs", 22, Bitfield32, None),
    w("CmdRealCurrent", 24, Int16, Some("A_SF")),
    w("CmdReactiveCurrent", 25, Int16, Some("A_SF")),
    r("RealCurrent", 26, Int16, Some("A_SF")),
    r("ReactiveCurrent", 27, Int16, Some("A_SF")),
    r("LocVAC", 28, Uint16, Some("V_SF")),
    r("RemPhVphA", 29, Uint16, Some("V_SF")),
    r("RemPhVphB", 30, Uint16, Some("V_SF")),
    r("RemPhVphC", 31, Uint16, Some("V_SF")),
    r("RemVAC", 32, Uint16, Some("V_SF")),
    r("RemHz", 33, Uint16, Some("HZ_SF")),
    r("TMP_SF", 34, Sunssf, None),
    r("TmpInt", 35, Int16, Some("TMP_SF")),
    r("TmpInlet", 36, Int16, Some("TMP_SF")),
    r("RemPPVphCA", 37, Uint16, Some("V_SF")),
    r("RemPPVphAB", 38, Uint16, Some("V_SF")),
    r("RemPPVphBC", 39, Uint16, Some("V_SF")),
    w("DcILim", 40, Uint16, Some("A_SF")),
    w("DcVLim", 41, Uint16, Some("V_SF")),
    w("LineILim", 42, Uint16, Some("A_SF")),
    w("InputCurrent", 43, Int16, Some("A_SF")),
];

/// The point `id` of model `model_id`, if the gateway ships that model and
/// the model has that point.
pub fn point(model_id: u16, id: &str) -> Option<&'static PointDef> {
    let points = match model_id {
        1 => COMMON,
        103 => INVERTER_THREE_PHASE,
        65534 => EPC_CONTROL,
        _ => return None,
    };
    points.iter().find(|p| p.id == id)
}
