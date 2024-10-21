use revm::wiring::result::HaltReason;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MantleHaltReason {
    Base(HaltReason),
    FailedDeposit,
}

impl From<HaltReason> for MantleHaltReason {
    fn from(value: HaltReason) -> Self {
        Self::Base(value)
    }
}
