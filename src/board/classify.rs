//! Heuristic classification of parts (by reference designator) and nets (by
//! name). Board files carry no component types, so these are educated
//! guesses based on common naming conventions, including the laptop
//! convention of prefixing power-section parts with `P` (PR, PC, PL, PQ, PU).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Resistor,
    Capacitor,
    Inductor,
    Diode,
    Transistor,
    Ic,
    Connector,
    Crystal,
    Fuse,
    Switch,
    TestPoint,
    /// Zero-ohm links: solder jumpers and shorting pads (JP, SL, SP).
    Jumper,
    /// Mounting holes, fiducials, shields.
    Mechanical,
    Other,
}

/// Refines a name-based guess with the pin count: a two-pin "transistor" is
/// a thermistor in some vendors' naming (e.g. PTR on ASUS boards).
pub fn refine_part_kind(kind: PartKind, pin_count: usize) -> PartKind {
    match kind {
        PartKind::Transistor if pin_count == 2 => PartKind::Resistor,
        other => other,
    }
}

/// Classifies a part from its reference designator.
pub fn classify_part(name: &str) -> PartKind {
    let upper = name.trim().to_ascii_uppercase();
    let prefix: String = upper.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if prefix.is_empty() {
        return PartKind::Other;
    }
    if let Some(kind) = kind_for_prefix(&prefix) {
        return kind;
    }
    // Power-section convention: PR12, PC305, PQ4, PU2, PL1, PD3, PJ1, PF1.
    if let Some(rest) = prefix.strip_prefix('P')
        && let Some(kind) = kind_for_prefix(rest)
    {
        return kind;
    }
    PartKind::Other
}

fn kind_for_prefix(prefix: &str) -> Option<PartKind> {
    use PartKind::*;
    Some(match prefix {
        "R" | "RN" | "RP" | "RA" | "RT" | "VR" | "RF" | "RS" | "TH" | "NTC" => Resistor,
        "C" | "CE" | "CT" | "EC" | "TC" | "BC" | "CI" | "CO" => Capacitor,
        "L" | "FB" | "FL" | "BEAD" | "SLN" | "CM" | "CMC" => Inductor,
        "D" | "LED" | "ZD" | "DZ" | "VD" | "CR" | "TVS" => Diode,
        "Q" | "TR" | "M" | "QH" | "QL" => Transistor,
        "U" | "IC" | "IU" => Ic,
        "J" | "CN" | "CON" | "JK" | "P" | "SK" => Connector,
        "JP" | "SL" | "SP" | "SJ" | "XW" | "NT" => Jumper,
        "X" | "Y" | "XTAL" | "OSC" | "XT" => Crystal,
        "F" | "FS" | "PF" => Fuse,
        "SW" | "S" | "K" | "BTN" => Switch,
        "TP" | "T" | "PAD" | "TPAD" | "VT" => TestPoint,
        "H" | "MH" | "HOLE" | "FID" | "FM" | "SH" | "SHD" | "EMI" => Mechanical,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetKind {
    Ground,
    Power,
    Signal,
    Unconnected,
}

/// Tokens that mark a net as a control or monitoring signal *about* a rail
/// (enable, power-good, sense, feedback) rather than the rail itself.
const SIGNAL_TOKENS: &[&str] = &[
    "EN", "ENA", "ENABLE", "PG", "PGOOD", "PWRGD", "PWROK", "GOOD", "OK", "ON", "OFF", "SNS", "SENSE", "FB", "CTRL",
    "CTL", "SEL", "DET", "ALERT", "INT", "SW", "PHASE", "LX", "BOOT", "BST", "DRV", "UG", "LG", "UGATE", "LGATE", "ID",
    "CS", "ISEN", "VID", "SDA", "SCL", "CLK", "RST", "RESET", "DIS", "N", "P", "DP", "DM",
];

/// Prefixes that name a supply rail in most vendors' schematics.
const POWER_PREFIXES: &[&str] = &[
    "VCC", "VDD", "VIN", "VBAT", "VBUS", "VCORE", "VREG", "VPP", "VSYS", "AVDD", "DVDD", "AVCC", "DVCC", "PVDD",
    "VDDQ", "VTT", "VREF", "VGFX", "VCCIO", "VCCSA", "VCCST", "VCCGT", "VNN", "VADP", "DCIN", "B+", "PPBUS", "PPVBAT",
    "PPDCIN", "PP_", "VAA",
];

/// Substrings that name a main system/input rail regardless of tokenization.
const POWER_NAME_PARTS: &[&str] =
    &["BAT_SYS", "AC_BAT", "VSYS", "DCIN", "DC_IN", "ADP_IN", "ADPIN", "VADP", "VBAT", "PPBUS", "SYS_PWR", "PWR_SRC"];

/// Classifies a net from its name.
pub fn classify_net(name: &str) -> NetKind {
    let upper = name.trim().to_ascii_uppercase();
    if upper.is_empty() || upper.starts_with("UNCONNECTED") || upper == "NC" || upper.starts_with("NC_") {
        return NetKind::Unconnected;
    }
    let tokens: Vec<&str> = upper.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();

    if tokens.iter().any(|t| is_ground_token(t)) && !tokens.iter().any(|t| SIGNAL_TOKENS.contains(t)) {
        return NetKind::Ground;
    }
    if tokens.iter().any(|t| SIGNAL_TOKENS.contains(t)) {
        return NetKind::Signal;
    }

    // Apple: PP3V3_S5, PPBUS_G3H. Asus/Quanta/Compal: +3VS, +5VALW, +VCCIO.
    let looks_power = upper.starts_with('+')
        || POWER_NAME_PARTS.iter().any(|p| upper.contains(p))
        || (upper.starts_with("PP") && upper[2..].starts_with(|c: char| c.is_ascii_digit()))
        || POWER_PREFIXES.iter().any(|p| upper.starts_with(p))
        || tokens.iter().any(|t| is_voltage_token(t));
    if looks_power { NetKind::Power } else { NetKind::Signal }
}

fn is_ground_token(token: &str) -> bool {
    matches!(token, "GND" | "GROUND" | "VSS" | "AGND" | "DGND" | "PGND" | "SGND" | "GNDA" | "GNDD" | "CGND" | "EGND")
        || (token.ends_with("GND") && token.len() <= 6)
}

/// Matches voltage-looking tokens: 5V, 12V, 3V3, 1V05, 3VS, 5VALW, P3V3,
/// V1P8 and 1P05V.
fn is_voltage_token(token: &str) -> bool {
    fn digits(s: &str) -> usize {
        s.bytes().take_while(u8::is_ascii_digit).count()
    }
    // <digits> V (<digits> | <up to 4 letters>)
    fn digits_v(s: &str) -> bool {
        let lead = digits(s);
        if lead == 0 || !s[lead..].starts_with('V') {
            return false;
        }
        let rest = &s[lead + 1..];
        rest.bytes().all(|b| b.is_ascii_digit()) || (rest.len() <= 4 && rest.bytes().all(|b| b.is_ascii_alphabetic()))
    }
    // <digits> P <digits>
    fn digits_p_digits(s: &str) -> bool {
        let lead = digits(s);
        lead > 0 && s[lead..].starts_with('P') && s.len() > lead + 1 && digits(&s[lead + 1..]) == s.len() - lead - 1
    }

    digits_v(token)
        || token.strip_prefix('P').is_some_and(digits_v)
        || token.strip_prefix('V').is_some_and(digits_p_digits)
        || token.strip_suffix('V').is_some_and(digits_p_digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts() {
        assert_eq!(classify_part("C123"), PartKind::Capacitor);
        assert_eq!(classify_part("PC305"), PartKind::Capacitor);
        assert_eq!(classify_part("PR12"), PartKind::Resistor);
        assert_eq!(classify_part("PQ4"), PartKind::Transistor);
        assert_eq!(classify_part("PU2"), PartKind::Ic);
        assert_eq!(classify_part("U7000"), PartKind::Ic);
        assert_eq!(classify_part("FB12"), PartKind::Inductor);
        assert_eq!(classify_part("CN1"), PartKind::Connector);
        assert_eq!(classify_part("Y1"), PartKind::Crystal);
        assert_eq!(classify_part("TP5"), PartKind::TestPoint);
        assert_eq!(classify_part("vt8"), PartKind::TestPoint);
        assert_eq!(classify_part("PJP8601"), PartKind::Jumper);
        assert_eq!(classify_part("SL1908"), PartKind::Jumper);
        assert_eq!(classify_part("PCI8001"), PartKind::Capacitor);
        assert_eq!(classify_part("PQH8901"), PartKind::Transistor);
        assert_eq!(classify_part("H6403"), PartKind::Mechanical);
        assert_eq!(refine_part_kind(classify_part("PTR5501"), 2), PartKind::Resistor);
        assert_eq!(classify_part("ZZ9"), PartKind::Other);
        assert_eq!(classify_part("123"), PartKind::Other);
    }

    #[test]
    fn nets() {
        for ground in ["GND", "AGND", "PGND", "GND_AUDIO", "VSS"] {
            assert_eq!(classify_net(ground), NetKind::Ground, "{ground}");
        }
        for power in [
            "AC_BAT_SYS",
            "PP3V3_S5",
            "PPBUS_G3H",
            "+3VS",
            "+5VALW",
            "+VCCIO",
            "VCC_CORE",
            "1V8_AON",
            "P3V3",
            "VDDQ",
            "V1P8",
            "1P05V",
        ] {
            assert_eq!(classify_net(power), NetKind::Power, "{power}");
        }
        for signal in ["SMB_CLK", "EN_3V3", "PP3V3_S5_PGOOD", "+3VS_EN", "USB_DP", "GND_SENSE", "LPC_AD0"] {
            assert_eq!(classify_net(signal), NetKind::Signal, "{signal}");
        }
        assert_eq!(classify_net("UNCONNECTED"), NetKind::Unconnected);
    }
}
