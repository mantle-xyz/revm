//! Contains the concrete implementation of the [L2ChainProvider] trait for the client program.
use crate::executor::block_on;
use alloc::sync::Arc;
use alloy_consensus::Header;
use alloy_primitives::{Address, Bytes, B256};
use alloy_rlp::Decodable;
use anyhow::Result;
use kona_executor::TrieDBProvider;
use kona_mpt::{TrieHinter, TrieNode, TrieProvider};
use kona_preimage::{CommsClient, PreimageKey, PreimageKeyType};
use kona_proof::{
    errors::OracleProviderError, HintType,
};

/// The oracle-backed L2 chain provider for the client program.
#[derive(Debug, Clone)]
pub struct OracleL2ChainProvider<T: CommsClient> {
    /// The preimage oracle client.
    oracle: Arc<T>,
}

impl<T: CommsClient> OracleL2ChainProvider<T> {
    /// Creates a new [OracleL2ChainProvider] with the given boot information and oracle client.
    pub fn new(oracle: Arc<T>) -> Self {
        Self { oracle }
    }
}

impl<T: CommsClient> TrieProvider for OracleL2ChainProvider<T> {
    type Error = OracleProviderError;

    fn trie_node_by_hash(&self, key: B256) -> std::result::Result<kona_mpt::TrieNode, Self::Error> {
        // On L2, trie node preimages are stored as keccak preimage types in the oracle. We assume
        // that a hint for these preimages has already been sent, prior to this call.
        block_on(async move {
            TrieNode::decode(
                &mut self
                    .oracle
                    .get(PreimageKey::new(*key, PreimageKeyType::Keccak256))
                    .await
                    .map_err(OracleProviderError::Preimage)?
                    .as_ref(),
            )
                .map_err(OracleProviderError::Rlp)
        })
    }
}

impl<T: CommsClient> TrieHinter for OracleL2ChainProvider<T> {
    type Error = anyhow::Error;

    fn hint_trie_node(&self, hash: B256) -> Result<()> {
        block_on(async move {
            Ok(self
                .oracle
                .write(&HintType::L2StateNode.encode_with(&[hash.as_slice()]))
                .await?)
        })
    }

    fn hint_account_proof(&self, address: Address, block_number: u64) -> Result<()> {
        block_on(async move {
            Ok(self
                .oracle
                .write(
                    &HintType::L2AccountProof
                        .encode_with(&[block_number.to_be_bytes().as_ref(), address.as_slice()]),
                )
                .await?)
        })
    }

    fn hint_storage_proof(
        &self,
        address: alloy_primitives::Address,
        slot: alloy_primitives::U256,
        block_number: u64,
    ) -> Result<()> {
        block_on(async move {
            Ok(self
                .oracle
                .write(&HintType::L2AccountStorageProof.encode_with(&[
                    block_number.to_be_bytes().as_ref(),
                    address.as_slice(),
                    slot.to_be_bytes::<32>().as_ref(),
                ]))
                .await?)
        })
    }
}

impl<T: CommsClient> TrieDBProvider for OracleL2ChainProvider<T> {
    fn bytecode_by_hash(&self, hash: B256) -> Result<Bytes, OracleProviderError> {
        // Fetch the bytecode preimage from the caching oracle.
        block_on(async move {
            self.oracle
                .write(&HintType::L2Code.encode_with(&[hash.as_ref()]))
                .await
                .map_err(OracleProviderError::Preimage)?;

            self.oracle
                .get(PreimageKey::new(*hash, PreimageKeyType::Keccak256))
                .await
                .map(Into::into)
                .map_err(OracleProviderError::Preimage)
        })
    }

    fn header_by_hash(&self, hash: B256) -> Result<Header, OracleProviderError> {
        // Fetch the header from the caching oracle.
        block_on(async move {
            self.oracle
                .write(&HintType::L2BlockHeader.encode_with(&[hash.as_ref()]))
                .await
                .map_err(OracleProviderError::Preimage)?;

            let header_bytes = self
                .oracle
                .get(PreimageKey::new(*hash, PreimageKeyType::Keccak256))
                .await
                .map_err(OracleProviderError::Preimage)?;
            Header::decode(&mut header_bytes.as_slice()).map_err(OracleProviderError::Rlp)
        })
    }
}