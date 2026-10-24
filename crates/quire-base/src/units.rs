//! Length units used across the engine.
//!
//! * The document model stores typographic lengths in [`Twip`] (1/1440 inch)
//!   because that is what OOXML uses.
//! * Drawing geometry uses [`Emu`] (1/914400 inch), also from OOXML.
//! * Layout converts both to [`Pt`] (1/72 inch) and works in points.
//!
//! Keeping the unit in the type prevents the classic "was this pixels or
//! points?" bug when the code crosses layer boundaries.

use std::fmt;

/// A length in typographic points (1 pt = 1/72 inch).
#[derive(Copy, Clone, Debug, Default, PartialEq, PartialOrd)]
pub struct Pt(pub f64);

impl Pt {
    pub const ZERO: Self = Self(0.0);

    pub const fn new(value: f64) -> Self {
        Self(value)
    }

    pub fn from_twips(twips: i32) -> Self {
        Self(f64::from(twips) / 20.0)
    }

    pub fn from_emu(emu: i64) -> Self {
        Self(emu as f64 / 12_700.0)
    }

    pub fn to_twips(self) -> i32 {
        (self.0 * 20.0).round() as i32
    }

    pub fn to_emu(self) -> i64 {
        (self.0 * 12_700.0).round() as i64
    }

    pub fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }

    pub fn clamp_non_negative(self) -> Self {
        Self(self.0.max(0.0))
    }
}

impl fmt::Display for Pt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}pt", self.0)
    }
}

/// A length in twips (1/1440 inch).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Twip(pub i32);

impl Twip {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    pub fn to_pt(self) -> Pt {
        Pt::from_twips(self.0)
    }

    pub fn from_pt(pt: Pt) -> Self {
        Self(pt.to_twips())
    }
}

impl From<Pt> for Twip {
    fn from(pt: Pt) -> Self {
        Self::from_pt(pt)
    }
}

impl From<Twip> for Pt {
    fn from(twip: Twip) -> Self {
        twip.to_pt()
    }
}

/// A length in English Metric Units (1/914400 inch).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Emu(pub i64);

impl Emu {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn to_pt(self) -> Pt {
        Pt::from_emu(self.0)
    }

    pub fn from_pt(pt: Pt) -> Self {
        Self(pt.to_emu())
    }
}

impl From<Pt> for Emu {
    fn from(pt: Pt) -> Self {
        Self::from_pt(pt)
    }
}

impl From<Emu> for Pt {
    fn from(emu: Emu) -> Self {
        emu.to_pt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twips_round_trip() {
        assert_eq!(Pt::from_twips(1440), Pt(72.0));
        assert_eq!(Pt(72.0).to_twips(), 1440);
        assert_eq!(Pt::from_twips(-20), Pt(-1.0));
    }

    #[test]
    fn emu_round_trip() {
        assert_eq!(Pt::from_emu(914_400), Pt(72.0));
        assert_eq!(Pt(72.0).to_emu(), 914_400);
    }

    #[test]
    fn conversions_are_consistent() {
        let pt = Pt::new(12.5);
        let twip = Twip::from_pt(pt);
        assert!((twip.to_pt().0 - pt.0).abs() < 1e-9);
    }
}
