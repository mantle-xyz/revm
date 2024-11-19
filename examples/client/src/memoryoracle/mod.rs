//! Contains the host <-> client communication utilities.

use crate::hasher::BytesHasherBuilder;
use alloy_primitives::{keccak256, FixedBytes, B256};
use anyhow::{anyhow, Result as AnyhowResult};
use async_trait::async_trait;
// use itertools::Itertools;
use kona_preimage::{
    errors::PreimageOracleError, HintWriterClient, PreimageKey, PreimageKeyType,
    PreimageOracleClient,
};
// use kzg_rs::{get_kzg_settings, Blob as KzgRsBlob, Bytes48};
use rkyv::{Archive, Deserialize, Infallible, Serialize};
// use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// An in-memory HashMap that will serve as the oracle for the zkVM.
/// Rather than relying on a trusted host for data, the data in this oracle
/// is verified with the `verify()` function, and then is trusted for
/// the remainder of execution.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct InMemoryOracle {
    cache: HashMap<[u8; 32], Vec<u8>, BytesHasherBuilder>,
}

impl InMemoryOracle {
    /// Creates a new [InMemoryOracle] from the raw bytes passed into the zkVM.
    /// These values are deserialized using rkyv for zero copy deserialization.
    pub fn from_raw_bytes(input: Vec<u8>) -> Self {
        println!("cycle-tracker-start: in-memory-oracle-from-raw-bytes-archive");
        let archived = unsafe { rkyv::archived_root::<Self>(&input) };
        println!("cycle-tracker-end: in-memory-oracle-from-raw-bytes-archive");
        println!("cycle-tracker-start: in-memory-oracle-from-raw-bytes-deserialize");
        let deserialized: Self = archived.deserialize(&mut Infallible).unwrap();
        println!("cycle-tracker-end: in-memory-oracle-from-raw-bytes-deserialize");

        deserialized
    }

    /// Creates a new [InMemoryOracle] from a HashMap of B256 keys and Vec<u8> values.
    pub fn from_b256_hashmap(data: HashMap<B256, Vec<u8>>) -> Self {
        let cache = data
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect::<HashMap<_, _, BytesHasherBuilder>>();
        Self { cache }
    }
}

#[async_trait]
impl PreimageOracleClient for InMemoryOracle {
    async fn get(&self, key: PreimageKey) -> Result<Vec<u8>, PreimageOracleError> {
        println!("cycle-tracker-start: in-memory-oracle-get");
        let lookup_key: [u8; 32] = key.into();
        self.cache
            .get(&lookup_key)
            .cloned()
            .ok_or_else(|| PreimageOracleError::KeyNotFound)
    }

    async fn get_exact(&self, key: PreimageKey, buf: &mut [u8]) -> Result<(), PreimageOracleError> {
        println!("cycle-tracker-start: in-memory-oracle-get-exact");
        let lookup_key: [u8; 32] = key.into();
        let value = self
            .cache
            .get(&lookup_key)
            .ok_or_else(|| PreimageOracleError::KeyNotFound)?;
        buf.copy_from_slice(value.as_slice());
        Ok(())
    }
}

#[async_trait]
impl HintWriterClient for InMemoryOracle {
    async fn write(&self, _hint: &str) -> Result<(), PreimageOracleError> {
        Ok(())
    }
}
