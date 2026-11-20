//! Who runs a mobile network, by its MCC-MNC (spec 022).
//!
//! Europe's large networks only, which is where an estate's modems are. A code
//! the table does not know is shown as the code — a short table is easier to
//! keep right than a long one, and a rebrand is a one-line change.

const CARRIERS: &[(&str, &str)] = &[
    ("20201", "Cosmote"),
    ("20205", "Vodafone GR"),
    ("20404", "Vodafone NL"),
    ("20408", "KPN"),
    ("20416", "Odido"),
    ("20601", "Proximus"),
    ("20610", "Orange BE"),
    ("20620", "Base"),
    ("20801", "Orange FR"),
    ("20810", "SFR"),
    ("20815", "Free Mobile"),
    ("20820", "Bouygues Telecom"),
    ("21401", "Vodafone ES"),
    ("21403", "Orange ES"),
    ("21407", "Movistar"),
    ("21630", "Telekom HU"),
    ("22201", "TIM"),
    ("22210", "Vodafone IT"),
    ("22288", "WindTre"),
    ("22601", "Vodafone RO"),
    ("22610", "Orange RO"),
    ("22801", "Swisscom"),
    ("22802", "Sunrise"),
    ("23001", "T-Mobile CZ"),
    ("23002", "O2 CZ"),
    ("23003", "Vodafone CZ"),
    ("23201", "A1"),
    ("23203", "Magenta Telekom"),
    ("23205", "Drei AT"),
    ("23410", "O2 UK"),
    ("23415", "Vodafone UK"),
    ("23420", "Three UK"),
    ("23430", "EE"),
    ("23801", "TDC"),
    ("23820", "Telia DK"),
    ("24001", "Telia SE"),
    ("24007", "Tele2"),
    ("24201", "Telenor NO"),
    ("24202", "Telia NO"),
    ("24405", "Elisa"),
    ("24491", "Telia FI"),
    ("26001", "Plus"),
    ("26002", "T-Mobile PL"),
    ("26003", "Orange PL"),
    ("26006", "Play"),
    ("26201", "Telekom Deutschland"),
    ("26202", "Vodafone Deutschland"),
    ("26203", "O2 Telefónica"),
    ("26207", "O2 Telefónica"),
    ("26801", "Vodafone PT"),
    ("26806", "MEO"),
    ("27001", "POST Luxembourg"),
    ("27201", "Vodafone IE"),
];

/// The carrier's name, when the table knows the network.
#[must_use]
pub fn name(network: &str) -> Option<&'static str> {
    CARRIERS
        .binary_search_by_key(&network, |(code, _)| code)
        .ok()
        .map(|index| CARRIERS[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_a_lookup_finds_every_row() {
        assert!(CARRIERS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(name("26201"), Some("Telekom Deutschland"));
        assert_eq!(name("99999"), None);
    }
}
