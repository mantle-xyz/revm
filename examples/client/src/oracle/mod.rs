use super::{hint::HintType, utils};
use alloc::{sync::Arc, vec::Vec};
use alloy::providers::Provider;
use alloy::{
    consensus::EMPTY_ROOT_HASH,
    eips::BlockId,
    providers::ReqwestProvider,
    rlp::EMPTY_STRING_CODE,
    rpc::types::{Block, BlockNumberOrTag, BlockTransactions, BlockTransactionsKind},
};
use alloy_primitives::{keccak256, Address, Bytes, B256};
use anyhow::Result;
use async_trait::async_trait;
use kona_preimage::{
    errors::PreimageOracleError, HintWriterClient, PreimageKey, PreimageKeyType,
    PreimageOracleClient,
};
use op_alloy_network::Optimism;
use spin::Mutex;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub struct MantleProviderOracle {
    provider: Arc<ReqwestProvider<Optimism>>,
    cache: Arc<Mutex<HashMap<PreimageKey, Vec<u8>>>>,
}

impl MantleProviderOracle {
    pub fn new(provider: Arc<ReqwestProvider<Optimism>>, cache_size: usize) -> Self {
        Self {
            provider,
            cache: Arc::new(Mutex::new(HashMap::with_capacity(cache_size))),
        }
    }
}

impl MantleProviderOracle {
    async fn store_trie_nodes<T: AsRef<[u8]>>(
        &self,
        nodes: &[T],
    ) -> Result<(), PreimageOracleError> {
        let mut kv_write_lock = self.cache.lock();

        // If the list of nodes is empty, store the empty root hash and exit early.
        // The `HashBuilder` will not push the preimage of the empty root hash to the
        // `ProofRetainer` in the event that there are no leaves inserted.
        if nodes.is_empty() {
            let empty_key = PreimageKey::new(*EMPTY_ROOT_HASH, PreimageKeyType::Keccak256);
            kv_write_lock.insert(empty_key, [EMPTY_STRING_CODE].into());
        }

        let mut hb = kona_mpt::ordered_trie_with_encoder(nodes, |node, buf| {
            buf.put_slice(node.as_ref());
        });
        hb.root();
        let intermediates = hb.take_proof_nodes().into_inner();

        for (_, value) in intermediates.into_iter() {
            let value_hash = keccak256(value.as_ref());
            let key = PreimageKey::new(*value_hash, PreimageKeyType::Keccak256);

            kv_write_lock.insert(key, value.into());
        }

        Ok(())
    }
}

#[async_trait]
impl PreimageOracleClient for MantleProviderOracle {
    async fn get(&self, key: PreimageKey) -> Result<Vec<u8>, PreimageOracleError> {
        let cache_lock = self.cache.lock();
        cache_lock
            .get(&key)
            .cloned()
            .ok_or_else(|| PreimageOracleError::KeyNotFound)
    }

    async fn get_exact(&self, key: PreimageKey, buf: &mut [u8]) -> Result<(), PreimageOracleError> {
        let cache_lock = self.cache.lock();
        // let mut cache_lock_clone = cache_lock.clone();
        if let Some(value) = cache_lock.get(&key) {
            buf.copy_from_slice(value.as_slice());
            Ok(())
        } else {
            Err(PreimageOracleError::KeyNotFound)
        }
    }
}

#[async_trait]
impl HintWriterClient for MantleProviderOracle {
    async fn write(&self, hint: &str) -> Result<(), PreimageOracleError> {
        let (hint_type, hint_data) =
            utils::parse_hint(hint).map_err(|e| PreimageOracleError::Other(e.to_string()))?;
        // println!("hint_type: {hint_type} and hint_data: {hint_data}");
        match hint_type {
            HintType::L2BlockHeader => {
                // Validate the hint data length.
                if hint_data.len() != 32 {
                    "invalid hint data length".to_string();
                }

                // Fetch the raw header from the L2 chain provider.
                let hash: B256 = hint_data.as_ref().try_into().map_err(|_| {
                    PreimageOracleError::Other("Failed to convert bytes to B256".to_string())
                })?;
                let raw_header: Bytes = self
                    .provider
                    .client()
                    .request("debug_getRawHeader", [hash])
                    .await
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to fetch header RLP".to_string())
                    })?;

                // Acquire a lock on the key-value store and set the preimage.
                let mut kv_lock = self.cache.lock();
                kv_lock.insert(
                    PreimageKey::new(*hash, PreimageKeyType::Keccak256),
                    raw_header.into(),
                );
            }
            HintType::L2Transactions => {
                // Validate the hint data length.
                if hint_data.len() != 32 {
                    "invalid hint data length".to_string();
                }

                // Fetch the block from the L2 chain provider and store the transactions within its
                // body in the key-value store.
                let hash: B256 = hint_data.as_ref().try_into().map_err(|_| {
                    PreimageOracleError::Other("Failed to convert bytes to B256".to_string())
                })?;
                let Block { transactions, .. } = self
                    .provider
                    .get_block_by_hash(hash, BlockTransactionsKind::Hashes)
                    .await
                    .map_err(|_| PreimageOracleError::Other("Failed to fetch block".to_string()))?
                    .ok_or(PreimageOracleError::Other("Block not found".to_string()))?;

                match transactions {
                    BlockTransactions::Hashes(transactions) => {
                        let mut encoded_transactions = Vec::with_capacity(transactions.len());
                        for tx_hash in transactions {
                            let tx = self
                                .provider
                                .client()
                                .request::<&[B256; 1], Bytes>("debug_getRawTransaction", &[tx_hash])
                                .await
                                .map_err(|_| {
                                    PreimageOracleError::Other(
                                        "Failed to fetch \
                                transaction"
                                            .to_string(),
                                    )
                                })?;
                            encoded_transactions.push(tx);
                        }

                        self.store_trie_nodes(encoded_transactions.as_slice())
                            .await?;
                    }
                    _ => {
                        "Block transactions not found".to_string();
                    }
                };
            }
            HintType::L2Code => {
                // geth hashdb scheme code hash key prefix
                const CODE_PREFIX: u8 = b'c';

                if hint_data.len() != 32 {
                    "invalid hint data length".to_string();
                }

                let hash: B256 = hint_data.as_ref().try_into().map_err(|_| {
                    PreimageOracleError::Other("Failed to convert bytes to B256".to_string())
                })?;

                // Attempt to fetch the code from the L2 chain provider.
                let code_hash = [&[CODE_PREFIX], hash.as_slice()].concat();
                let code = self
                    .provider
                    .client()
                    .request::<&[Bytes; 1], Bytes>("debug_dbGet", &[code_hash.into()])
                    .await;

                // Check if the first attempt to fetch the code failed. If it did, try fetching the
                // code hash preimage without the geth hashdb scheme prefix.
                let code = match code {
                    Ok(code) => code,
                    Err(_) => self
                        .provider
                        .client()
                        .request::<&[B256; 1], Bytes>("debug_dbGet", &[hash])
                        .await
                        .map_err(|_| {
                            PreimageOracleError::Other("Failed to fetch code".to_string())
                        })?,
                };

                let mut kv_write_lock = self.cache.lock();
                kv_write_lock.insert(
                    PreimageKey::new(*hash, PreimageKeyType::Keccak256),
                    code.into(),
                );
            }
            HintType::StartingL2Output => {
                unimplemented!();
            }
            HintType::L2StateNode => {
                if hint_data.len() != 32 {
                    "invalid hint data length".to_string();
                }

                let hash: B256 = hint_data.as_ref().try_into().map_err(|_| {
                    PreimageOracleError::Other("Failed to convert bytes to B256".to_string())
                })?;

                // Fetch the preimage from the L2 chain provider.
                let preimage: Bytes = self
                    .provider
                    .client()
                    .request("debug_dbGet", &[hash])
                    .await
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to fetch state node".to_string())
                    })?;

                let mut kv_write_lock = self.cache.lock();
                kv_write_lock.insert(
                    PreimageKey::new(*hash, PreimageKeyType::Keccak256),
                    preimage.into(),
                );
            }
            HintType::L2AccountProof => {
                if hint_data.len() != 8 + 20 {
                    "invalid hint data length".to_string();
                }

                let block_number =
                    u64::from_be_bytes(hint_data.as_ref()[..8].try_into().map_err(|_| {
                        PreimageOracleError::Other("Failed to convert hint data to u64".to_string())
                    })?);
                let address = Address::from_slice(&hint_data.as_ref()[8..28]);
                let proof_response = self
                    .provider
                    .get_proof(address, Default::default())
                    .block_id(BlockId::Number(BlockNumberOrTag::Number(block_number)))
                    .await
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to fetch account proof".to_string())
                    })?;
                let mut kv_write_lock = self.cache.lock();

                // Write the account proof nodes to the key-value store.
                proof_response
                    .account_proof
                    .into_iter()
                    .try_for_each(|node| {
                        let node_hash = keccak256(node.as_ref());
                        let key = PreimageKey::new(*node_hash, PreimageKeyType::Keccak256);
                        kv_write_lock.insert(key.into(), node.into());
                        Ok::<(), PreimageOracleError>(())
                    })
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to store account proof over".to_string())
                    })?;
                drop(kv_write_lock);
            }
            HintType::L2AccountStorageProof => {
                if hint_data.len() != 8 + 20 + 32 {
                    "invalid hint data length".to_string();
                }

                let block_number =
                    u64::from_be_bytes(hint_data.as_ref()[..8].try_into().map_err(|_| {
                        PreimageOracleError::Other(
                            "Failed to convert hint data to u64".to_string(),
                        )
                    })?);
                let address = Address::from_slice(&hint_data.as_ref()[8..28]);
                let slot = B256::from_slice(&hint_data.as_ref()[28..]);
                let mut proof_response =
                    timeout(Duration::from_secs(10),
                            self
                                .provider
                                .get_proof(address, vec![slot])
                                .block_id(BlockId::Number(BlockNumberOrTag::Number(block_number))),
                    ).await
                        .map_err(|_| PreimageOracleError::Other("Storage proof request timed out".to_string()))?
                        .map_err(|_| {
                            println!("Failed to fetch storage proof");
                            PreimageOracleError::Other("Failed to fetch storage proof".to_string())
                        })?;
                let mut kv_write_lock = self.cache.lock();

                // Write the account proof nodes to the key-value store.
                proof_response
                    .account_proof
                    .into_iter()
                    .try_for_each(|node| {
                        let node_hash = keccak256(node.as_ref());
                        let key = PreimageKey::new(*node_hash, PreimageKeyType::Keccak256);
                        kv_write_lock.insert(key, node.into());
                        Ok::<(), PreimageOracleError>(())
                    })
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to store account proof".to_string())
                    })?;

                // Write the storage proof nodes to the key-value store.
                let storage_proof = proof_response.storage_proof.remove(0);
                storage_proof
                    .proof
                    .into_iter()
                    .try_for_each(|node| {
                        let node_hash = keccak256(node.as_ref());
                        let key = PreimageKey::new(*node_hash, PreimageKeyType::Keccak256);
                        kv_write_lock.insert(key, node.into());
                        Ok::<(), PreimageOracleError>(())
                    })
                    .map_err(|_| {
                        PreimageOracleError::Other("Failed to store storage proof".to_string())
                    })?;
                drop(kv_write_lock);
            }
        }
        Ok(())
    }
}
