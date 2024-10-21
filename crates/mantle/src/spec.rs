use revm::{
    precompile::PrecompileSpecId,
    specification::hardfork::{Spec, SpecId},
};

/// Specification IDs for the mantle blockchain.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, enumn::N)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(non_camel_case_types)]
pub enum MantleSpecId {
    FRONTIER = 0,
    FRONTIER_THAWING = 1,
    HOMESTEAD = 2,
    DAO_FORK = 3,
    TANGERINE = 4,
    SPURIOUS_DRAGON = 5,
    BYZANTIUM = 6,
    CONSTANTINOPLE = 7,
    PETERSBURG = 8,
    ISTANBUL = 9,
    MUIR_GLACIER = 10,
    BERLIN = 11,
    LONDON = 12,
    ARROW_GLACIER = 13,
    GRAY_GLACIER = 14,
    MERGE = 15,
    BEDROCK = 16,
    REGOLITH = 17,
    SHANGHAI = 18,
    CANYON = 19,
    CANCUN = 20,
    ECOTONE = 21,
    FJORD = 22,
    GRANITE = 23,
    PRAGUE = 24,
    PRAGUE_EOF = 25,
    #[default]
    LATEST = u8::MAX,
}

impl MantleSpecId {
    /// Returns the `MantleSpecId` for the given `u8`.
    #[inline]
    pub fn try_from_u8(spec_id: u8) -> Option<Self> {
        Self::n(spec_id)
    }

    /// Returns `true` if the given specification ID is enabled in this spec.
    #[inline]
    pub const fn is_enabled_in(self, other: Self) -> bool {
        Self::enabled(self, other)
    }

    /// Returns `true` if the given specification ID is enabled in this spec.
    #[inline]
    pub const fn enabled(our: Self, other: Self) -> bool {
        our as u8 >= other as u8
    }

    /// Converts the `MantleSpecId` into a `SpecId`.
    const fn into_eth_spec_id(self) -> SpecId {
        match self {
            MantleSpecId::FRONTIER => SpecId::FRONTIER,
            MantleSpecId::FRONTIER_THAWING => SpecId::FRONTIER_THAWING,
            MantleSpecId::HOMESTEAD => SpecId::HOMESTEAD,
            MantleSpecId::DAO_FORK => SpecId::DAO_FORK,
            MantleSpecId::TANGERINE => SpecId::TANGERINE,
            MantleSpecId::SPURIOUS_DRAGON => SpecId::SPURIOUS_DRAGON,
            MantleSpecId::BYZANTIUM => SpecId::BYZANTIUM,
            MantleSpecId::CONSTANTINOPLE => SpecId::CONSTANTINOPLE,
            MantleSpecId::PETERSBURG => SpecId::PETERSBURG,
            MantleSpecId::ISTANBUL => SpecId::ISTANBUL,
            MantleSpecId::MUIR_GLACIER => SpecId::MUIR_GLACIER,
            MantleSpecId::BERLIN => SpecId::BERLIN,
            MantleSpecId::LONDON => SpecId::LONDON,
            MantleSpecId::ARROW_GLACIER => SpecId::ARROW_GLACIER,
            MantleSpecId::GRAY_GLACIER => SpecId::GRAY_GLACIER,
            MantleSpecId::MERGE | MantleSpecId::BEDROCK | MantleSpecId::REGOLITH => {
                SpecId::MERGE
            }
            MantleSpecId::SHANGHAI | MantleSpecId::CANYON => SpecId::SHANGHAI,
            MantleSpecId::CANCUN
            | MantleSpecId::ECOTONE
            | MantleSpecId::FJORD
            | MantleSpecId::GRANITE => SpecId::CANCUN,
            MantleSpecId::PRAGUE => SpecId::PRAGUE,
            MantleSpecId::PRAGUE_EOF => SpecId::PRAGUE_EOF,
            MantleSpecId::LATEST => SpecId::LATEST,
        }
    }
}

impl From<MantleSpecId> for SpecId {
    fn from(value: MantleSpecId) -> Self {
        value.into_eth_spec_id()
    }
}

impl From<SpecId> for MantleSpecId {
    fn from(value: SpecId) -> Self {
        match value {
            SpecId::FRONTIER => Self::FRONTIER,
            SpecId::FRONTIER_THAWING => Self::FRONTIER_THAWING,
            SpecId::HOMESTEAD => Self::HOMESTEAD,
            SpecId::DAO_FORK => Self::DAO_FORK,
            SpecId::TANGERINE => Self::TANGERINE,
            SpecId::SPURIOUS_DRAGON => Self::SPURIOUS_DRAGON,
            SpecId::BYZANTIUM => Self::BYZANTIUM,
            SpecId::CONSTANTINOPLE => Self::CONSTANTINOPLE,
            SpecId::PETERSBURG => Self::PETERSBURG,
            SpecId::ISTANBUL => Self::ISTANBUL,
            SpecId::MUIR_GLACIER => Self::MUIR_GLACIER,
            SpecId::BERLIN => Self::BERLIN,
            SpecId::LONDON => Self::LONDON,
            SpecId::ARROW_GLACIER => Self::ARROW_GLACIER,
            SpecId::GRAY_GLACIER => Self::GRAY_GLACIER,
            SpecId::MERGE => Self::MERGE,
            SpecId::SHANGHAI => Self::SHANGHAI,
            SpecId::CANCUN => Self::CANCUN,
            SpecId::PRAGUE => Self::PRAGUE,
            SpecId::PRAGUE_EOF => Self::PRAGUE_EOF,
            SpecId::LATEST => Self::LATEST,
        }
    }
}

impl From<MantleSpecId> for PrecompileSpecId {
    fn from(value: MantleSpecId) -> Self {
        PrecompileSpecId::from_spec_id(value.into_eth_spec_id())
    }
}

/// String identifiers for Mantle hardforks.
pub mod id {
    // Re-export the Ethereum hardforks.
    pub use revm::specification::hardfork::id::*;

    pub const BEDROCK: &str = "Bedrock";
    pub const REGOLITH: &str = "Regolith";
    pub const CANYON: &str = "Canyon";
    pub const ECOTONE: &str = "Ecotone";
    pub const FJORD: &str = "Fjord";
    pub const GRANITE: &str = "Granite";
}

impl From<&str> for MantleSpecId {
    fn from(name: &str) -> Self {
        match name {
            id::FRONTIER => Self::FRONTIER,
            id::FRONTIER_THAWING => Self::FRONTIER_THAWING,
            id::HOMESTEAD => Self::HOMESTEAD,
            id::DAO_FORK => Self::DAO_FORK,
            id::TANGERINE => Self::TANGERINE,
            id::SPURIOUS_DRAGON => Self::SPURIOUS_DRAGON,
            id::BYZANTIUM => Self::BYZANTIUM,
            id::CONSTANTINOPLE => Self::CONSTANTINOPLE,
            id::PETERSBURG => Self::PETERSBURG,
            id::ISTANBUL => Self::ISTANBUL,
            id::MUIR_GLACIER => Self::MUIR_GLACIER,
            id::BERLIN => Self::BERLIN,
            id::LONDON => Self::LONDON,
            id::ARROW_GLACIER => Self::ARROW_GLACIER,
            id::GRAY_GLACIER => Self::GRAY_GLACIER,
            id::MERGE => Self::MERGE,
            id::SHANGHAI => Self::SHANGHAI,
            id::CANCUN => Self::CANCUN,
            id::PRAGUE => Self::PRAGUE,
            id::PRAGUE_EOF => Self::PRAGUE_EOF,
            id::BEDROCK => Self::BEDROCK,
            id::REGOLITH => Self::REGOLITH,
            id::CANYON => Self::CANYON,
            id::ECOTONE => Self::ECOTONE,
            id::FJORD => Self::FJORD,
            id::LATEST => Self::LATEST,
            _ => Self::LATEST,
        }
    }
}

impl From<MantleSpecId> for &'static str {
    fn from(value: MantleSpecId) -> Self {
        match value {
            MantleSpecId::FRONTIER
            | MantleSpecId::FRONTIER_THAWING
            | MantleSpecId::HOMESTEAD
            | MantleSpecId::DAO_FORK
            | MantleSpecId::TANGERINE
            | MantleSpecId::SPURIOUS_DRAGON
            | MantleSpecId::BYZANTIUM
            | MantleSpecId::CONSTANTINOPLE
            | MantleSpecId::PETERSBURG
            | MantleSpecId::ISTANBUL
            | MantleSpecId::MUIR_GLACIER
            | MantleSpecId::BERLIN
            | MantleSpecId::LONDON
            | MantleSpecId::ARROW_GLACIER
            | MantleSpecId::GRAY_GLACIER
            | MantleSpecId::MERGE
            | MantleSpecId::SHANGHAI
            | MantleSpecId::CANCUN
            | MantleSpecId::PRAGUE
            | MantleSpecId::PRAGUE_EOF => value.into_eth_spec_id().into(),
            MantleSpecId::BEDROCK => id::BEDROCK,
            MantleSpecId::REGOLITH => id::REGOLITH,
            MantleSpecId::CANYON => id::CANYON,
            MantleSpecId::ECOTONE => id::ECOTONE,
            MantleSpecId::FJORD => id::FJORD,
            MantleSpecId::GRANITE => id::GRANITE,
            MantleSpecId::LATEST => id::LATEST,
        }
    }
}

pub trait MantleSpec: Spec + Sized + 'static {
    /// The specification ID for mantle.
    const MANTLE_SPEC_ID: MantleSpecId;

    /// Returns whether the provided `MantleSpec` is enabled by this spec.
    #[inline]
    fn mantle_enabled(spec_id: MantleSpecId) -> bool {
        MantleSpecId::enabled(Self::MANTLE_SPEC_ID, spec_id)
    }
}

macro_rules! spec {
    ($spec_id:ident, $spec_name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $spec_name;

        impl MantleSpec for $spec_name {
            const MANTLE_SPEC_ID: MantleSpecId = MantleSpecId::$spec_id;
        }

        impl Spec for $spec_name {
            const SPEC_ID: SpecId = $spec_name::MANTLE_SPEC_ID.into_eth_spec_id();
        }
    };
}

spec!(FRONTIER, FrontierSpec);
// FRONTIER_THAWING no EVM spec change
spec!(HOMESTEAD, HomesteadSpec);
// DAO_FORK no EVM spec change
spec!(TANGERINE, TangerineSpec);
spec!(SPURIOUS_DRAGON, SpuriousDragonSpec);
spec!(BYZANTIUM, ByzantiumSpec);
// CONSTANTINOPLE was overridden with PETERSBURG
spec!(PETERSBURG, PetersburgSpec);
spec!(ISTANBUL, IstanbulSpec);
// MUIR_GLACIER no EVM spec change
spec!(BERLIN, BerlinSpec);
spec!(LONDON, LondonSpec);
// ARROW_GLACIER no EVM spec change
// GRAY_GLACIER no EVM spec change
spec!(MERGE, MergeSpec);
spec!(SHANGHAI, ShanghaiSpec);
spec!(CANCUN, CancunSpec);
spec!(PRAGUE, PragueSpec);
spec!(PRAGUE_EOF, PragueEofSpec);

spec!(LATEST, LatestSpec);

// Mantle Hardforks
spec!(BEDROCK, BedrockSpec);
spec!(REGOLITH, RegolithSpec);
spec!(CANYON, CanyonSpec);
spec!(ECOTONE, EcotoneSpec);
spec!(FJORD, FjordSpec);
spec!(GRANITE, GraniteSpec);

#[macro_export]
macro_rules! mantle_spec_to_generic {
    ($spec_id:expr, $e:expr) => {{
        // We are transitioning from var to generic spec.
        match $spec_id {
            $crate::MantleSpecId::FRONTIER | $crate::MantleSpecId::FRONTIER_THAWING => {
                use $crate::FrontierSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::HOMESTEAD | $crate::MantleSpecId::DAO_FORK => {
                use $crate::HomesteadSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::TANGERINE => {
                use $crate::TangerineSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::SPURIOUS_DRAGON => {
                use $crate::SpuriousDragonSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::BYZANTIUM => {
                use $crate::ByzantiumSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::PETERSBURG | $crate::MantleSpecId::CONSTANTINOPLE => {
                use $crate::PetersburgSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::ISTANBUL | $crate::MantleSpecId::MUIR_GLACIER => {
                use $crate::IstanbulSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::BERLIN => {
                use $crate::BerlinSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::LONDON
            | $crate::MantleSpecId::ARROW_GLACIER
            | $crate::MantleSpecId::GRAY_GLACIER => {
                use $crate::LondonSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::MERGE => {
                use $crate::MergeSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::SHANGHAI => {
                use $crate::ShanghaiSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::CANCUN => {
                use $crate::CancunSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::LATEST => {
                use $crate::LatestSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::PRAGUE => {
                use $crate::PragueSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::PRAGUE_EOF => {
                use $crate::PragueEofSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::BEDROCK => {
                use $crate::BedrockSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::REGOLITH => {
                use $crate::RegolithSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::CANYON => {
                use $crate::CanyonSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::GRANITE => {
                use $crate::GraniteSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::ECOTONE => {
                use $crate::EcotoneSpec as SPEC;
                $e
            }
            $crate::MantleSpecId::FJORD => {
                use $crate::FjordSpec as SPEC;
                $e
            }
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mantle_spec_to_generic() {
        mantle_spec_to_generic!(
            MantleSpecId::FRONTIER,
            assert_eq!(SPEC::SPEC_ID, SpecId::FRONTIER)
        );
        mantle_spec_to_generic!(
            MantleSpecId::FRONTIER_THAWING,
            assert_eq!(SPEC::SPEC_ID, SpecId::FRONTIER)
        );
        mantle_spec_to_generic!(
            MantleSpecId::HOMESTEAD,
            assert_eq!(SPEC::SPEC_ID, SpecId::HOMESTEAD)
        );
        mantle_spec_to_generic!(
            MantleSpecId::DAO_FORK,
            assert_eq!(SPEC::SPEC_ID, SpecId::HOMESTEAD)
        );
        mantle_spec_to_generic!(
            MantleSpecId::TANGERINE,
            assert_eq!(SPEC::SPEC_ID, SpecId::TANGERINE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::SPURIOUS_DRAGON,
            assert_eq!(SPEC::SPEC_ID, SpecId::SPURIOUS_DRAGON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BYZANTIUM,
            assert_eq!(SPEC::SPEC_ID, SpecId::BYZANTIUM)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CONSTANTINOPLE,
            assert_eq!(SPEC::SPEC_ID, SpecId::PETERSBURG)
        );
        mantle_spec_to_generic!(
            MantleSpecId::PETERSBURG,
            assert_eq!(SPEC::SPEC_ID, SpecId::PETERSBURG)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ISTANBUL,
            assert_eq!(SPEC::SPEC_ID, SpecId::ISTANBUL)
        );
        mantle_spec_to_generic!(
            MantleSpecId::MUIR_GLACIER,
            assert_eq!(SPEC::SPEC_ID, SpecId::ISTANBUL)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BERLIN,
            assert_eq!(SPEC::SPEC_ID, SpecId::BERLIN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::LONDON,
            assert_eq!(SPEC::SPEC_ID, SpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ARROW_GLACIER,
            assert_eq!(SPEC::SPEC_ID, SpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::GRAY_GLACIER,
            assert_eq!(SPEC::SPEC_ID, SpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::MERGE,
            assert_eq!(SPEC::SPEC_ID, SpecId::MERGE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BEDROCK,
            assert_eq!(SPEC::SPEC_ID, SpecId::MERGE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::REGOLITH,
            assert_eq!(SPEC::SPEC_ID, SpecId::MERGE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::SHANGHAI,
            assert_eq!(SPEC::SPEC_ID, SpecId::SHANGHAI)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CANYON,
            assert_eq!(SPEC::SPEC_ID, SpecId::SHANGHAI)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CANCUN,
            assert_eq!(SPEC::SPEC_ID, SpecId::CANCUN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ECOTONE,
            assert_eq!(SPEC::SPEC_ID, SpecId::CANCUN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::FJORD,
            assert_eq!(SPEC::SPEC_ID, SpecId::CANCUN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::PRAGUE,
            assert_eq!(SPEC::SPEC_ID, SpecId::PRAGUE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::LATEST,
            assert_eq!(SPEC::SPEC_ID, SpecId::LATEST)
        );
        mantle_spec_to_generic!(
            MantleSpecId::FRONTIER,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::FRONTIER)
        );
        mantle_spec_to_generic!(
            MantleSpecId::FRONTIER_THAWING,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::FRONTIER)
        );
        mantle_spec_to_generic!(
            MantleSpecId::HOMESTEAD,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::HOMESTEAD)
        );
        mantle_spec_to_generic!(
            MantleSpecId::DAO_FORK,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::HOMESTEAD)
        );
        mantle_spec_to_generic!(
            MantleSpecId::TANGERINE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::TANGERINE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::SPURIOUS_DRAGON,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::SPURIOUS_DRAGON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BYZANTIUM,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::BYZANTIUM)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CONSTANTINOPLE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::PETERSBURG)
        );
        mantle_spec_to_generic!(
            MantleSpecId::PETERSBURG,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::PETERSBURG)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ISTANBUL,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::ISTANBUL)
        );
        mantle_spec_to_generic!(
            MantleSpecId::MUIR_GLACIER,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::ISTANBUL)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BERLIN,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::BERLIN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::LONDON,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ARROW_GLACIER,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::GRAY_GLACIER,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::LONDON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::MERGE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::MERGE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::BEDROCK,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::BEDROCK)
        );
        mantle_spec_to_generic!(
            MantleSpecId::REGOLITH,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::REGOLITH)
        );
        mantle_spec_to_generic!(
            MantleSpecId::SHANGHAI,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::SHANGHAI)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CANYON,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::CANYON)
        );
        mantle_spec_to_generic!(
            MantleSpecId::CANCUN,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::CANCUN)
        );
        mantle_spec_to_generic!(
            MantleSpecId::ECOTONE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::ECOTONE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::FJORD,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::FJORD)
        );
        mantle_spec_to_generic!(
            MantleSpecId::GRANITE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::GRANITE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::PRAGUE,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::PRAGUE)
        );
        mantle_spec_to_generic!(
            MantleSpecId::PRAGUE_EOF,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::PRAGUE_EOF)
        );
        mantle_spec_to_generic!(
            MantleSpecId::LATEST,
            assert_eq!(SPEC::MANTLE_SPEC_ID, MantleSpecId::LATEST)
        );
    }

    #[test]
    fn test_bedrock_post_merge_hardforks() {
        assert!(BedrockSpec::mantle_enabled(MantleSpecId::MERGE));
        assert!(!BedrockSpec::mantle_enabled(MantleSpecId::SHANGHAI));
        assert!(!BedrockSpec::mantle_enabled(MantleSpecId::CANCUN));
        assert!(!BedrockSpec::mantle_enabled(MantleSpecId::LATEST));
        assert!(BedrockSpec::mantle_enabled(MantleSpecId::BEDROCK));
        assert!(!BedrockSpec::mantle_enabled(MantleSpecId::REGOLITH));
    }

    #[test]
    fn test_regolith_post_merge_hardforks() {
        assert!(RegolithSpec::mantle_enabled(MantleSpecId::MERGE));
        assert!(!RegolithSpec::mantle_enabled(MantleSpecId::SHANGHAI));
        assert!(!RegolithSpec::mantle_enabled(MantleSpecId::CANCUN));
        assert!(!RegolithSpec::mantle_enabled(MantleSpecId::LATEST));
        assert!(RegolithSpec::mantle_enabled(MantleSpecId::BEDROCK));
        assert!(RegolithSpec::mantle_enabled(MantleSpecId::REGOLITH));
    }

    #[test]
    fn test_bedrock_post_merge_hardforks_spec_id() {
        assert!(MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::MERGE
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::SHANGHAI
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::CANCUN
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::LATEST
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::BEDROCK
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::BEDROCK,
            MantleSpecId::REGOLITH
        ));
    }

    #[test]
    fn test_regolith_post_merge_hardforks_spec_id() {
        assert!(MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::MERGE
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::SHANGHAI
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::CANCUN
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::LATEST
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::BEDROCK
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::REGOLITH,
            MantleSpecId::REGOLITH
        ));
    }

    #[test]
    fn test_canyon_post_merge_hardforks() {
        assert!(CanyonSpec::mantle_enabled(MantleSpecId::MERGE));
        assert!(CanyonSpec::mantle_enabled(MantleSpecId::SHANGHAI));
        assert!(!CanyonSpec::mantle_enabled(MantleSpecId::CANCUN));
        assert!(!CanyonSpec::mantle_enabled(MantleSpecId::LATEST));
        assert!(CanyonSpec::mantle_enabled(MantleSpecId::BEDROCK));
        assert!(CanyonSpec::mantle_enabled(MantleSpecId::REGOLITH));
        assert!(CanyonSpec::mantle_enabled(MantleSpecId::CANYON));
    }

    #[test]
    fn test_canyon_post_merge_hardforks_spec_id() {
        assert!(MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::MERGE
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::SHANGHAI
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::CANCUN
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::LATEST
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::BEDROCK
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::REGOLITH
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::CANYON,
            MantleSpecId::CANYON
        ));
    }

    #[test]
    fn test_ecotone_post_merge_hardforks() {
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::MERGE));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::SHANGHAI));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::CANCUN));
        assert!(!EcotoneSpec::mantle_enabled(MantleSpecId::LATEST));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::BEDROCK));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::REGOLITH));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::CANYON));
        assert!(EcotoneSpec::mantle_enabled(MantleSpecId::ECOTONE));
    }

    #[test]
    fn test_ecotone_post_merge_hardforks_spec_id() {
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::MERGE
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::SHANGHAI
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::CANCUN
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::LATEST
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::BEDROCK
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::REGOLITH
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::CANYON
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::ECOTONE,
            MantleSpecId::ECOTONE
        ));
    }

    #[test]
    fn test_fjord_post_merge_hardforks() {
        assert!(FjordSpec::mantle_enabled(MantleSpecId::MERGE));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::SHANGHAI));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::CANCUN));
        assert!(!FjordSpec::mantle_enabled(MantleSpecId::LATEST));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::BEDROCK));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::REGOLITH));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::CANYON));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::ECOTONE));
        assert!(FjordSpec::mantle_enabled(MantleSpecId::FJORD));
    }

    #[test]
    fn test_fjord_post_merge_hardforks_spec_id() {
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::MERGE
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::SHANGHAI
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::CANCUN
        ));
        assert!(!MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::LATEST
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::BEDROCK
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::REGOLITH
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::CANYON
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::ECOTONE
        ));
        assert!(MantleSpecId::enabled(
            MantleSpecId::FJORD,
            MantleSpecId::FJORD
        ));
    }
}
