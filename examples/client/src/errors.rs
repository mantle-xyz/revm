//! Error types for the client program.

use alloc::string::String;
use derive_more::derive::Display;
use kona_mpt::OrderedListWalkerError;
use kona_preimage::errors::PreimageOracleError;

/// Error from an oracle-backed provider.
#[derive(Display, Debug)]
pub enum OracleProviderError {
    /// Requested block number is past the chain head.
    #[display("Block number ({_0}) past chain head ({_1})")]
    BlockNumberPastHead(u64, u64),
    /// Preimage oracle error.
    #[display("Preimage oracle error: {_0}")]
    Preimage(PreimageOracleError),
    /// List walker error.
    #[display("Trie walker error: {_0}")]
    TrieWalker(OrderedListWalkerError),
    /// BlockInfo error.
    // #[display("From block error: {_0}")]
    // BlockInfo(FromBlockError),
    /// Op Block conversion error.
    // #[display("Op block conversion error: {_0}")]
    // OpBlockConversion(OpBlockConversionError),
    /// Error decoding or encoding RLP.
    #[display("RLP error: {_0}")]
    Rlp(alloy_rlp::Error),
    /// Slice conversion error.
    #[display("Slice conversion error: {_0}")]
    SliceConversion(core::array::TryFromSliceError),
    /// Serde error.
    #[display("Serde error: {_0}")]
    Serde(serde_json::Error),
}

impl core::error::Error for OracleProviderError {}


/// Driver error.
#[derive(Display, Debug)]
pub enum DriverError {
    #[display("Oracle provider error: {_0}")]
    Oracle(OracleProviderError),
    /// Error parsing a hint.
    #[display("Hint parsing error: {_0}")]
    HintParsing(HintParsingError),
    /// Error decoding or encoding RLP.
    #[display("RLP error: {_0}")]
    Rlp(alloy_rlp::Error),
}

impl core::error::Error for DriverError {}

impl From<OracleProviderError> for DriverError {
    fn from(val: OracleProviderError) -> Self {
        DriverError::Oracle(val)
    }
}

impl From<HintParsingError> for DriverError {
    fn from(val: HintParsingError) -> Self {
        DriverError::HintParsing(val)
    }
}

impl From<alloy_rlp::Error> for DriverError {
    fn from(val: alloy_rlp::Error) -> Self {
        DriverError::Rlp(val)
    }
}

/// Error parsing a hint.
#[derive(Display, Debug)]
#[display("Hint parsing error: {_0}")]
pub struct HintParsingError(pub String);

impl core::error::Error for HintParsingError {}
