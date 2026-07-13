//! Classification of wireless device kinds from the GetDev record bytes.
//! Determines LED geometry, minimum PWM duty, and display names.

/// Wireless device kind.
///
/// Classification inputs (from the 42-byte device record):
/// - `device_type` byte [18]: 10/11 = AIOs, 1-9 = Strimer, 65/66/88 = case devices
/// - otherwise the per-slot fan-type bytes [24..28]:
///   SLV3 LED 20-23, SLV3 LCD 24-26, TLV2 LCD 27|32-35, TLV2 LED 28-31,
///   SL-INF 36-39, RL120 40, CLV1 41-42
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// SLV3 LED fans (no LCD) — 14% minimum duty, 40 LEDs/fan
    Slv3Led,
    /// SLV3 LCD fans — 14% minimum duty, 40 LEDs/fan
    Slv3Lcd,
    /// TLV2 LCD fans — 10% minimum duty, 26 LEDs/fan
    Tlv2Lcd,
    /// TLV2 LED fans — 11% minimum duty, 26 LEDs/fan
    Tlv2Led,
    /// SL-INF wireless fans — 11% minimum duty, 44 LEDs/fan
    SlInf,
    /// CL / RL120 fans — 10% minimum duty, 24 LEDs/fan (special PWM filter)
    Clv1,
    /// HydroShift II LCD-C (Circle) wireless AIO (device_type 10)
    WaterBlock,
    /// HydroShift II LCD-S / H2S (Square) wireless AIO (device_type 11)
    WaterBlock2,
    /// Strimer Wireless LED strip (device_type 1-9) — RGB only, no fans
    Strimer(u8),
    /// Lancool 217 case RGB ring (device_type 65) — 96 LEDs
    Lc217,
    /// Universal Screen 8.8" LED ring (device_type 88) — 88 LEDs
    Led88,
    /// Lancool V150 controller (device_type 66) — 88 LEDs dual-zone
    V150,
    /// Unknown device kind
    Unknown,
}

impl DeviceKind {
    /// Minimum PWM duty percentage the hardware accepts for a nonzero speed.
    pub fn min_duty_percent(self) -> u8 {
        match self {
            Self::Slv3Led | Self::Slv3Lcd => 14,
            Self::Tlv2Lcd => 10,
            Self::Tlv2Led | Self::SlInf => 11,
            Self::Clv1 | Self::WaterBlock | Self::WaterBlock2 | Self::V150 => 10,
            Self::Strimer(_) | Self::Lc217 | Self::Led88 => 0,
            Self::Unknown => 10,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Slv3Led => "UNI FAN SL V3 Wireless",
            Self::Slv3Lcd => "UNI FAN SL V3 Wireless LCD",
            Self::Tlv2Lcd => "UNI FAN TL Wireless LCD",
            Self::Tlv2Led => "UNI FAN TL Wireless",
            Self::SlInf => "UNI FAN SL-INF Wireless",
            Self::Clv1 => "UNI FAN CL Wireless",
            Self::WaterBlock => "HydroShift II LCD-C (Wireless)",
            Self::WaterBlock2 => "HydroShift II LCD-S (Wireless)",
            Self::Strimer(_) => "Strimer Wireless",
            Self::Lc217 => "Lancool 217 Wireless",
            Self::Led88 => "Universal Screen 8.8\" Wireless",
            Self::V150 => "Lancool V150 Wireless",
            Self::Unknown => "Wireless Device",
        }
    }

    pub fn leds_per_fan(self) -> u8 {
        match self {
            Self::Tlv2Lcd | Self::Tlv2Led => 26,
            Self::Slv3Led | Self::Slv3Lcd => 40,
            Self::SlInf => 44,
            Self::Clv1 | Self::WaterBlock | Self::WaterBlock2 => 24,
            Self::Strimer(_) | Self::Lc217 | Self::Led88 | Self::V150 => 0,
            Self::Unknown => 20,
        }
    }

    pub fn is_aio(self) -> bool {
        matches!(self, Self::WaterBlock | Self::WaterBlock2)
    }

    pub fn is_rgb_only(self) -> bool {
        matches!(self, Self::Strimer(_) | Self::Lc217 | Self::Led88)
    }

    pub fn pump_led_count(self) -> u8 {
        if self.is_aio() {
            24
        } else {
            0
        }
    }

    /// Total LED count for flat-buffer (non per-fan) devices.
    pub fn led_count_override(self) -> Option<u16> {
        match self {
            Self::Strimer(dt) => Some(match dt {
                1 => 116,
                2 => 132,
                3 => 174,
                _ => 88,
            }),
            Self::Lc217 => Some(96),
            Self::Led88 => Some(88),
            Self::V150 => Some(88),
            _ => None,
        }
    }

    /// Classify from a per-slot fan-type byte in the device record.
    pub fn from_fan_type_byte(b: u8) -> Self {
        match b {
            20..=23 => Self::Slv3Led,
            24..=26 => Self::Slv3Lcd,
            27 | 32..=35 => Self::Tlv2Lcd,
            28..=31 => Self::Tlv2Led,
            36..=39 => Self::SlInf,
            40..=42 => Self::Clv1,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sl_inf_classification_and_geometry() {
        for b in 36..=39u8 {
            assert_eq!(DeviceKind::from_fan_type_byte(b), DeviceKind::SlInf);
        }
        assert_eq!(DeviceKind::SlInf.leds_per_fan(), 44);
        assert_eq!(DeviceKind::SlInf.min_duty_percent(), 11);
        assert!(!DeviceKind::SlInf.is_rgb_only());
    }

    #[test]
    fn strimer_led_counts_by_subtype() {
        assert_eq!(DeviceKind::Strimer(1).led_count_override(), Some(116));
        assert_eq!(DeviceKind::Strimer(2).led_count_override(), Some(132));
        assert_eq!(DeviceKind::Strimer(3).led_count_override(), Some(174));
        assert_eq!(DeviceKind::Strimer(9).led_count_override(), Some(88));
        assert!(DeviceKind::Strimer(2).is_rgb_only());
        assert_eq!(DeviceKind::Strimer(2).min_duty_percent(), 0);
    }

    #[test]
    fn boundary_bytes() {
        assert_eq!(DeviceKind::from_fan_type_byte(19), DeviceKind::Unknown);
        assert_eq!(DeviceKind::from_fan_type_byte(20), DeviceKind::Slv3Led);
        assert_eq!(DeviceKind::from_fan_type_byte(27), DeviceKind::Tlv2Lcd);
        assert_eq!(DeviceKind::from_fan_type_byte(28), DeviceKind::Tlv2Led);
        assert_eq!(DeviceKind::from_fan_type_byte(43), DeviceKind::Unknown);
    }
}
