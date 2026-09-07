//! The two identifiers a Plug & Charge contract is assembled from.
//!
//! One of them already exists: [`emob_core::Emaid`] parses all three contract
//! grammars, verifies the check digit against the right one of two algorithms,
//! and preserves the text it was given. Nothing here re-implements it.
//!
//! The other is the **vehicle's**, and it had no owner: the PCID that names one
//! car in one OEM's production, carried as the Common Name of its provisioning
//! certificate. It is the key the whole ecosystem is arranged around — a
//! provisioning certificate is stored under it, contracts are assigned to it,
//! and deleting it deletes them `[OPCP §Provisioning Certificate Pool]` — and
//! it is the one identifier here that no other crate in this workspace has a
//! reason to know.

use core::fmt;

/// A Provisioning Certificate Identifier: one vehicle, in one OEM's production.
///
/// `[OPCP §Handling of IDs]` gives the grammar as an ABNF from
/// VDE-AR-E 2802-100-1 and, in the same paragraph, as the expression the
/// ecosystem applies — *"the WMI (3 Characters) of the OEM, followed by 14
/// alphanumeric characters which uniquely identifies the vehicle and an
/// optional check digit"*, matched against `^[a-zA-Z0-9]{17,18}$`.
///
/// # The WMI is a method because it decides authority
///
/// The Provisioning Certificate Pool checks it against the OEM account's
/// authorised list before it stores anything, and the Contract Certificate Pool
/// pushes a new contract to the OEM enrolled for it. Holding the id as a string
/// and slicing it at the call site writes that rule down in as many places as it
/// is asked.
///
/// # Case
///
/// Upper-cased, and the text is not preserved: the ecosystem *"will normalize to
/// the form above (no seperators and captial letters)"*, so two spellings are
/// one vehicle. The opposite of [`emob_core::Emaid`]'s choice, and deliberately
/// — an eMAID is quoted back to a driver, a PCID is a pool lookup.
///
/// ```
/// use emob_pnc::Pcid;
///
/// let pcid: Pcid = "hubjectopenpnc0008".parse()?;
/// assert_eq!(pcid.as_str(), "HUBJECTOPENPNC0008");
/// assert_eq!(pcid.wmi(), "HUB");
/// assert!("HUBJECTOPENPNC00".parse::<Pcid>().is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Pcid(String);

/// The shortest PCID `[OPCP §Handling of IDs]` accepts — WMI plus fourteen.
const PCID_MIN: usize = 17;
/// The longest — the same, with the optional check digit.
const PCID_MAX: usize = 18;
/// ISO 3780's World Manufacturer Identifier, which is the PCID's first three.
const WMI_LEN: usize = 3;

impl Pcid {
    /// Read a PCID, upper-casing it.
    ///
    /// # Errors
    ///
    /// [`PcidError::Length`] outside seventeen to eighteen characters, and
    /// [`PcidError::NotAlphanumeric`] for anything that is not `[0-9A-Z]` after
    /// upper-casing — which is where a separator lands, because the ecosystem
    /// strips those rather than accepting them.
    pub fn new(raw: &str) -> Result<Self, PcidError> {
        let upper = raw.to_ascii_uppercase();
        if !(PCID_MIN..=PCID_MAX).contains(&upper.chars().count()) {
            return Err(PcidError::Length {
                pcid: upper,
                len: raw.chars().count(),
            });
        }
        if let Some(offending) = upper.chars().find(|c| !c.is_ascii_alphanumeric()) {
            return Err(PcidError::NotAlphanumeric {
                pcid: upper,
                offending,
            });
        }
        Ok(Self(upper))
    }

    /// The identifier, normalised.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The World Manufacturer Identifier — ISO 3780, the first three
    /// characters.
    ///
    /// The OEM this vehicle belongs to, and therefore which OEM account may
    /// publish its provisioning certificate and which OEM is notified when a
    /// contract is written against it `[OPCP §Provisioning Certificate Pool]`.
    #[must_use]
    pub fn wmi(&self) -> &str {
        &self.0[..WMI_LEN]
    }
}

impl fmt::Display for Pcid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::str::FromStr for Pcid {
    type Err = PcidError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::new(raw)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Pcid {
    /// Through [`Pcid::new`], because a `derive` would restore a `Pcid` from
    /// any string at all — including one that is not a PCID and one that is not
    /// normalised, which is two vehicles where the ecosystem holds one.
    ///
    /// The rule this workspace keeps re-learning (D264): a value arrives
    /// through a door, and the door is not only the constructor.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// Why a string is not a PCID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PcidError {
    /// Not seventeen or eighteen characters.
    #[error(
        "{pcid} is {len} characters and a PCID is 17 or 18 — a World Manufacturer Identifier, \
         fourteen characters naming the vehicle, and an optional check digit \
         [OPCP §Handling of IDs]"
    )]
    Length {
        /// What was offered, upper-cased.
        pcid: String,
        /// How long it was.
        len: usize,
    },

    /// Something in it is not `[0-9A-Z]`.
    #[error(
        "{pcid} contains {offending:?}, and a PCID is alphanumeric: the ecosystem normalises \
         separators away rather than accepting them [OPCP §Handling of IDs]"
    )]
    NotAlphanumeric {
        /// What was offered, upper-cased.
        pcid: String,
        /// The first character that is not admissible.
        offending: char,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_hubject_provisioning_identifier_parses_and_names_its_manufacturer() {
        // The Common Name of `hubjectopenpnc0008.pem`, from Hubject's own
        // published test environment.
        let pcid = Pcid::new("hubjectopenpnc0008").expect("18 alphanumeric characters");
        assert_eq!(pcid.as_str(), "HUBJECTOPENPNC0008");
        assert_eq!(pcid.wmi(), "HUB");
    }

    #[test]
    fn seventeen_characters_is_a_pcid_without_its_check_digit() {
        let pcid = Pcid::new("WVW12345678901234").expect("17 characters");
        assert_eq!(pcid.wmi(), "WVW");
    }

    #[test]
    fn a_length_that_is_not_a_pcid_is_refused_by_name() {
        let short = Pcid::new("WVW1234567890123").expect_err("16 characters");
        assert!(
            matches!(short, PcidError::Length { len: 16, .. }),
            "{short}"
        );
        let long = Pcid::new("WVW1234567890123456").expect_err("19 characters");
        assert!(matches!(long, PcidError::Length { len: 19, .. }), "{long}");
    }

    #[test]
    fn a_separator_is_refused_rather_than_stripped() {
        // The ecosystem normalises separators *away*; a value that still has
        // one has not been through that normalisation, and quietly deleting it
        // here would accept two spellings of one vehicle as one — while a
        // registry somewhere else, keyed on the raw text, held two.
        let error = Pcid::new("HUB-JECTOPENPNC008").expect_err("a hyphen");
        assert!(
            matches!(error, PcidError::NotAlphanumeric { offending: '-', .. }),
            "{error}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn nothing_reaches_a_pcid_past_its_own_constructor() {
        let json = r#""hubjectopenpnc0008""#;
        let pcid: Pcid = serde_json::from_str(json).expect("a valid PCID");
        assert_eq!(
            pcid.as_str(),
            "HUBJECTOPENPNC0008",
            "normalised on the way in"
        );
        assert!(
            serde_json::from_str::<Pcid>(r#""nope""#).is_err(),
            "a derive would have restored this one"
        );
    }
}
