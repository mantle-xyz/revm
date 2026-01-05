//! Example that show how to replay a block and trace the execution of each transaction.
//!
//! The EIP3155 trace of each transaction is saved into file `traces/{tx_number}.json`.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy_consensus::{transaction::SignerRecoverable, TxEip1559, TxEip2930, TxEip7702, TxLegacy};
use alloy_eips::{BlockId, Decodable2718, Typed2718};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_provider::{network::primitives::BlockTransactions, Provider, ProviderBuilder};
use alloy_rpc_types::eth::EIP1186AccountProofResponse;
use dotenv::dotenv;
use op_alloy_consensus::{OpTxEnvelope, TxDeposit};
use op_alloy_network::Optimism;
use op_revm::{
    api::{builder::OpBuilder, default_ctx::DefaultOp},
    spec::OpSpecId,
    transaction::deposit::DepositTransactionParts,
    OpTransaction,
};
use revm::{
    context::tx::TxEnv,
    context_interface::either::Either,
    database::{AlloyDB, CacheDB, StateBuilder},
    database_interface::WrapDatabaseAsync,
    primitives::{TxKind, KECCAK_EMPTY},
    Context, ExecuteCommitEvm,
};
use std::time::Instant;
use std::fs;
use std::path::Path;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Set up the HTTP transport which is consumed by the RPC client.
    dotenv().ok();
    let mantle_url = std::env::var("MANTLE_URL").unwrap();
    let chain_id = std::env::var("CHAIN_ID").unwrap().parse()?;
    let rpc_url = mantle_url.parse()?;

    // Create a provider
    let client = ProviderBuilder::<_, _, Optimism>::default().connect_http(rpc_url);

    // Params
    let start_block = std::env::var("START_BLOCK")
        .expect("START_BLOCK must be set")
        .parse::<u64>()?;
    let end_block = std::env::var("END_BLOCK")
        .expect("END_BLOCK must be set")
        .parse::<u64>()?;
    let spec = std::env::var("OP_SPEC")
        .unwrap_or_else(|_| "Isthmus".to_string())
        .parse::<OpSpecId>()
        .expect("Invalid OP_SPEC value. Valid values: Bedrock, Regolith, Canyon, Ecotone, Fjord, Granite, Holocene, Isthmus, Jovian, Interop, Osaka");

    // Check if state verification is enabled
    let state_verify = std::env::var("STATE_VERIFY")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase()
        == "true";

    // Check if cache_db export is enabled
    // Default to false if not set
    let export_cache_db = std::env::var("EXPORT_CACHE_DB")
        .unwrap_or_else(|_| "false".to_string())
        .to_lowercase()
        == "true";
    
    if export_cache_db {
        println!("⚠️  EXPORT_CACHE_DB is enabled - cache_db will be exported");
    }

    for i in start_block..=end_block {
        println!("Processing block number: {i}");
        process_block(i, chain_id, spec, state_verify, export_cache_db, client.clone()).await?;
    }

    Ok(())
}

async fn process_block(
    block_number: u64,
    chain_id: u64,
    spec: OpSpecId,
    state_verify: bool,
    export_cache_db: bool,
    client: impl Provider<Optimism> + Clone,
) -> anyhow::Result<()> {
    // Fetch the transaction-rich block
    let block = client
        .get_block_by_number(block_number.into())
        .await
        .expect("Failed to get parent block")
        .expect("Block not found");

    println!("Fetched block number: {block_number}");
    let previous_block_number = block_number - 1;

    // Use the previous block state as the db with caching
    let prev_id: BlockId = previous_block_number.into();
    // SAFETY: This cannot fail since this is in the top-level tokio runtime

    let state_db = WrapDatabaseAsync::new(AlloyDB::new(client.clone(), prev_id)).unwrap();
    let cache_db: CacheDB<_> = CacheDB::new(state_db);
    let mut state = StateBuilder::new_with_database(cache_db).build();
    let ctx = Context::op()
        .with_db(&mut state)
        .modify_block_chained(|b| {
            b.number = U256::from(block.header.number);
            b.beneficiary = block.header.beneficiary;
            b.timestamp = U256::from(block.header.timestamp);

            b.difficulty = block.header.difficulty;
            b.gas_limit = block.header.gas_limit;
            b.basefee = block.header.base_fee_per_gas.unwrap_or_default();
        })
        .modify_cfg_chained(|c| {
            c.chain_id = chain_id;
            c.spec = spec;
        });

    let mut evm = ctx.build_op();

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    // let console_bar = Arc::new(ProgressBar::new(txs as u64));
    let start = Instant::now();

    // Fill in CfgEnv
    let BlockTransactions::Hashes(transactions) = block.transactions else {
        panic!("Wrong transaction type")
    };

    for tx_hash in transactions.iter() {
        println!("tx_hash: {tx_hash}");
        let raw_tx = client
            .clone()
            .client()
            .request::<&[B256; 1], Bytes>("debug_getRawTransaction", &[*tx_hash])
            .await
            .expect("Block not found");
        let tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref()).unwrap();

        let optx = prepare_tx_env(&tx, tx.recover_signer().unwrap(), raw_tx);
        evm.0.modify_tx(|etx| {
            *etx = optx;
        });

        let is_deposit = tx.is_deposit();
        println!("is_deposit: {is_deposit}");

        let res = evm.replay_commit();

        if let Err(ref res) = res {
            println!("Got error: {res:?}");
        }

        let expected_gas_used = client
            .clone()
            .get_transaction_receipt(*tx_hash)
            .await
            .unwrap()
            .unwrap()
            .inner
            .gas_used;

        let actual_gas_used = res.unwrap().gas_used();
        println!("Expected gas used: {expected_gas_used}, Actual gas used: {actual_gas_used}");
        if expected_gas_used == actual_gas_used {
            println!("--- passed✅");
        } else {
            println!("--- failed❌");
        }
    }

    // Verify account states using eth_getProof if enabled
    if state_verify {
        if let Err(e) = verify_storage_with_proof(&state, client.clone(), block_number).await {
            println!("⚠️  Error during verification: {}", e);
        }
    }

    // Export cache_db data if enabled
    if export_cache_db {
        if let Err(e) = export_cache_db_data(&state, block_number).await {
            println!("⚠️  Error during cache_db export: {}", e);
        } else {
            println!("--- cache_db exported✅");
        }
    }

    let elapsed = start.elapsed();
    println!(
        "Finished block {block_number}. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// Export cache_db data to JSON file
async fn export_cache_db_data<P: alloy_provider::Provider<op_alloy_network::Optimism> + Clone>(
    state: &revm::database::State<revm::database::CacheDB<revm::database_interface::WrapDatabaseAsync<revm::database::AlloyDB<op_alloy_network::Optimism, P>>>>,
    block_number: u64,
) -> anyhow::Result<()> {
    // Create output directory if it doesn't exist
    let output_dir = Path::new("cache_db_exports");
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)?;
    }

    // Access CacheDB's cache directly since State.database is pub
    // Cache already supports serde serialization when serde feature is enabled
    // Serialize the cache to JSON
    let json = serde_json::to_string_pretty(&state.database.cache)?;
    
    // Write to file
    let file_path = output_dir.join(format!("block_{}.json", block_number));
    fs::write(&file_path, json)?;
    
    println!("Exported cache_db data to: {}", file_path.display());
    
    Ok(())
}

/// Prepare the transaction environment for the given transaction.
pub fn prepare_tx_env(tx: &OpTxEnvelope, caller: Address, encoded: Bytes) -> OpTransaction<TxEnv> {
    let base = match tx {
        OpTxEnvelope::Legacy(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip1559(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip2930(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Eip7702(tx) => tx.tx().to_tx_env(caller),
        OpTxEnvelope::Deposit(tx) => {
            let TxDeposit {
                to,
                value,
                gas_limit,
                input,
                source_hash: _,
                from: _,
                mint: _,
                is_system_transaction: _,
                eth_value: _,
                eth_tx_value: _,
            } = tx.inner();
            TxEnv {
                tx_type: tx.ty(),
                caller,
                gas_limit: *gas_limit,
                kind: *to,
                value: *value,
                data: input.clone(),
                ..Default::default()
            }
        }
    };

    let deposit = if let OpTxEnvelope::Deposit(tx) = tx {
        DepositTransactionParts {
            source_hash: tx.source_hash,
            mint: Some(tx.mint),
            is_system_transaction: tx.is_system_transaction,
            eth_value: Some(tx.eth_value),
            eth_tx_value: tx.eth_tx_value,
        }
    } else {
        Default::default()
    };

    OpTransaction {
        base,
        enveloped_tx: Some(encoded),
        deposit,
    }
}

trait ToTxEnv {
    fn to_tx_env(&self, caller: Address) -> TxEnv;
}

impl ToTxEnv for TxLegacy {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: self.chain_id,
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip1559 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.max_fee_per_gas,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: Some(self.chain_id),
            gas_priority_fee: Some(self.max_priority_fee_per_gas),
            access_list: self.access_list.clone(),
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip2930 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.gas_price,
            kind: self.to,
            value: self.value,
            data: self.input.clone(),
            chain_id: Some(self.chain_id),
            nonce: self.nonce,
            access_list: self.access_list.clone(),
            ..Default::default()
        }
    }
}

impl ToTxEnv for TxEip7702 {
    fn to_tx_env(&self, caller: Address) -> TxEnv {
        TxEnv {
            tx_type: self.ty(),
            caller,
            gas_limit: self.gas_limit,
            gas_price: self.max_fee_per_gas,
            kind: TxKind::Call(self.to),
            value: self.value,
            data: self.input.clone(),
            nonce: self.nonce,
            chain_id: Some(self.chain_id),
            gas_priority_fee: Some(self.max_priority_fee_per_gas),
            access_list: self.access_list.clone(),
            authorization_list: self
                .authorization_list
                .iter()
                .map(|auth| Either::Left(auth.clone()))
                .collect(),
            ..Default::default()
        }
    }
}

// ============================================================================
// Block State Verification Functions
// ============================================================================

/// Batch verify account storage slots and state using eth_getProof
async fn verify_storage_with_proof<DB>(
    state: &revm::database::State<DB>,
    client: impl Provider<Optimism> + Clone,
    block_number: u64,
) -> anyhow::Result<()> {
    let block_id: BlockId = block_number.into();
    let accounts = &state.cache.accounts;

    let mut total_failed = 0;
    let mut balance_mismatches = 0;
    let mut nonce_mismatches = 0;
    let mut code_hash_mismatches = 0;

    for (address, cache_account) in accounts {
        if let Some(account) = &cache_account.account {
            // Collect all storage slot keys (batch query)
            let storage_keys: Vec<B256> = account.storage.keys().map(|k| B256::from(*k)).collect();

            // Call eth_getProof once to get account state and all storage slot proofs
            let proof_result: Result<EIP1186AccountProofResponse, _> = client
                .get_proof(*address, storage_keys.clone())
                .block_id(block_id)
                .await;

            match proof_result {
                Ok(proof) => {
                    // Verify basic account information
                    if proof.balance != account.info.balance {
                        balance_mismatches += 1;
                        println!("  ❌ Balance mismatch for {}: remote={}, local={}", 
                            address, proof.balance, account.info.balance);
                    }
                    if proof.nonce != account.info.nonce {
                        nonce_mismatches += 1;
                        println!("  ❌ Nonce mismatch for {}: remote={}, local={}", 
                            address, proof.nonce, account.info.nonce);
                    }
                    
                    // Compare code_hash with compatibility for empty code representations
                    // Both KECCAK_EMPTY and zero hash represent "no code"
                    let remote_is_empty = proof.code_hash == KECCAK_EMPTY || proof.code_hash == B256::ZERO;
                    let local_is_empty = account.info.code_hash == KECCAK_EMPTY || account.info.code_hash == B256::ZERO;
                    
                    if !(remote_is_empty && local_is_empty) && proof.code_hash != account.info.code_hash {
                        code_hash_mismatches += 1;
                        println!("  ❌ Code hash mismatch for {}: remote={}, local={}", 
                            address, proof.code_hash, account.info.code_hash);
                    }

                    // Verify each storage slot (if any)
                    if !storage_keys.is_empty() {
                        for storage_proof in &proof.storage_proof {
                            let key = storage_proof.key;
                            let value_from_proof = storage_proof.value;

                            let key_str = format!("{:?}", key);
                            let key_str_clean =
                                key_str.trim_start_matches("Hash(0x").trim_end_matches(")");

                            if let Ok(key_u256) = U256::from_str_radix(key_str_clean, 16) {
                                if let Some(cached_value) = account.storage.get(&key_u256) {
                                    if value_from_proof != *cached_value {
                                        total_failed += 1;
                                        println!("  ❌ Storage mismatch for {} at slot {}: remote={}, local={}", 
                                            address, key, value_from_proof, cached_value);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("⚠️  Failed to get proof for account {}: {}", address, e);
                }
            }
        }
    }

    // Print concise verification result (similar to gas used verification)
    let all_match = total_failed == 0 && balance_mismatches == 0 && nonce_mismatches == 0 && code_hash_mismatches == 0;

    if all_match {
        println!("--- State verification: passed✅");
    } else {
        println!("--- State verification: failed❌");
        println!("  Balance mismatches: {}, Nonce mismatches: {}, Code hash mismatches: {}, Storage mismatches: {}", 
            balance_mismatches, nonce_mismatches, code_hash_mismatches, total_failed);
    }

    Ok(())
}
