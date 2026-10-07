use std::{fmt, str::FromStr};

// ---------------------------------------------------------------------------
// Ocean (server)
// ---------------------------------------------------------------------------

/// A live Puzzle Pirates ocean (server). Defunct oceans (Sage, Hunter,
/// Malachite, Viridian, Midnight, Cobalt) are deliberately not represented:
/// they no longer host yoweb or market data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ocean {
    Emerald,
    Meridian,
    Cerulean,
    Obsidian,
    Opal,
    Jade,
    Ice,
}

impl Ocean {
    /// The defunct ocean names we recognise only to give a helpful error.
    const DEFUNCT: [&'static str; 6] = [
        "sage",
        "hunter",
        "malachite",
        "viridian",
        "midnight",
        "cobalt",
    ];
    /// All live oceans, in canonical order (the ones with market data first).
    pub const LIVE: [Ocean; 7] = [
        Ocean::Emerald,
        Ocean::Meridian,
        Ocean::Cerulean,
        Ocean::Obsidian,
        Ocean::Opal,
        Ocean::Jade,
        Ocean::Ice,
    ];

    /// Display name, e.g. `"Emerald"`.
    pub fn name(self) -> &'static str {
        match self {
            Ocean::Emerald => "Emerald",
            Ocean::Meridian => "Meridian",
            Ocean::Cerulean => "Cerulean",
            Ocean::Obsidian => "Obsidian",
            Ocean::Opal => "Opal",
            Ocean::Jade => "Jade",
            Ocean::Ice => "Ice",
        }
    }

    /// yoweb subdomain (lowercase ocean name).
    pub fn subdomain(self) -> &'static str {
        match self {
            Ocean::Emerald => "emerald",
            Ocean::Meridian => "meridian",
            Ocean::Cerulean => "cerulean",
            Ocean::Obsidian => "obsidian",
            Ocean::Opal => "opal",
            Ocean::Jade => "jade",
            Ocean::Ice => "ice",
        }
    }

    /// Base URL for this ocean's yoweb (pirate stats) pages.
    pub fn yoweb_base(self) -> String {
        format!(
            "https://{}.puzzlepirates.com/yoweb",
            self.subdomain()
        )
    }

    /// Whether the market API serves prices for this ocean.
    pub fn market_supported(self) -> bool {
        matches!(
            self,
            Ocean::Emerald | Ocean::Meridian | Ocean::Cerulean
        )
    }
}

impl fmt::Display for Ocean {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Ocean {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.trim().to_ascii_lowercase();
        for ocean in Ocean::LIVE {
            if ocean.subdomain() == lower {
                return Ok(ocean);
            }
        }
        if Ocean::DEFUNCT.contains(&lower.as_str()) {
            return Err(format!(
                "'{s}' is a defunct ocean and is no longer supported"
            ));
        }
        Err(format!(
            "unknown ocean '{s}' (expected one of: {})",
            Ocean::LIVE
                .iter()
                .map(|o| o.name())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_is_case_insensitive() {
        assert_eq!(
            "emerald".parse::<Ocean>().unwrap(),
            Ocean::Emerald
        );
        assert_eq!(
            "EMERALD".parse::<Ocean>().unwrap(),
            Ocean::Emerald
        );
        assert_eq!(
            "  Ice  ".parse::<Ocean>().unwrap(),
            Ocean::Ice
        );
        assert_eq!(
            "MeRiDiAn".parse::<Ocean>().unwrap(),
            Ocean::Meridian
        );
    }

    #[test]
    fn defunct_oceans_are_rejected_with_context() {
        let err = "midnight".parse::<Ocean>().unwrap_err();
        assert!(err.contains("defunct"), "got: {err}");
        assert!("cobalt".parse::<Ocean>().is_err());
    }

    #[test]
    fn unknown_oceans_are_rejected() {
        assert!("atlantis".parse::<Ocean>().is_err());
    }

    #[test]
    fn only_three_oceans_have_market_prices() {
        for ocean in Ocean::LIVE {
            let expected = matches!(
                ocean,
                Ocean::Emerald | Ocean::Meridian | Ocean::Cerulean
            );
            assert_eq!(
                ocean.market_supported(),
                expected,
                "{ocean}"
            );
        }
    }

    #[test]
    fn yoweb_base_uses_lowercase_subdomain() {
        assert_eq!(
            Ocean::Cerulean.yoweb_base(),
            "https://cerulean.puzzlepirates.com/yoweb"
        );
        assert_eq!(Ocean::Ice.subdomain(), "ice");
        assert_eq!(Ocean::Ice.name(), "Ice");
    }
}
