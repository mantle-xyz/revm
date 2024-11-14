//! Optimism-specific constants, types, and helpers.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy::{
    eips::{eip2718::Decodable2718, BlockNumberOrTag},
    providers::{Provider, ProviderBuilder},
};
use alloy_consensus::Header;
use alloy_primitives::{Bytes, Sealable};
use anyhow::{anyhow, Result};
use client::{mantle::OracleL2ChainProvider, oracle::MantleProviderOracle};
use dotenv::dotenv;
use kona_executor::TrieDB;
use op_alloy_consensus::OpTxEnvelope;
use op_alloy_network::Optimism;
use revm::primitives::{OptimismFields, SpecId, TransactTo, TxEnv, TxKind, B256, U256};
use revm::{
    db::{states::bundle_state::BundleRetention, State},
    EvmBuilder,
    SEQUENCER_FEE_VAULT_ADDRESS,
};
use revm_primitives::bitvec::macros::internal::funty::Fundamental;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

struct CheckerRecord {
    total: u64,
    passed: u64,
    failed: u64,
}
impl CheckerRecord {
    fn new() -> Self {
        Self {
            total: 0,
            passed: 0,
            failed: 0,
        }
    }
    fn add(&mut self, passed: bool) {
        self.total += 1;
        if passed {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
    }
    fn print(&self) {
        println!(
            "Total: {}, Passed: {}, Failed: {}, {}% correct",
            self.total,
            self.passed,
            self.failed,
            self.passed as f64 / self.total as f64 * 100.0
        );
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start = 71632023;
    // let start = 71301230;
    dotenv().ok();
    let record = Arc::new(Mutex::new(CheckerRecord::new()));
    for block_number in start..start + 1 {
        range(block_number, record.clone()).await?;
    }
    record.lock().unwrap().print();
    Ok(())
}

async fn range(block_number: u64, record: Arc<Mutex<CheckerRecord>>) -> anyhow::Result<()> {
    let chain_id: u64 = 5000;
    let mantle_url = std::env::var("MANTLE_URL").unwrap();

    let url = mantle_url.as_str();
    let client = ProviderBuilder::new()
        .network::<Optimism>()
        .on_http(url.parse().unwrap());
    let client = Arc::new(client);

    let prev_block = client
        .get_block_by_number(BlockNumberOrTag::from(block_number - 1), true)
        .await
        .unwrap()
        .ok_or(anyhow!("Block not found"))
        .unwrap();
    //
    let block = client
        .get_block_by_number(BlockNumberOrTag::from(block_number), true)
        .await
        .unwrap()
        .ok_or(anyhow!("Block not found"))
        .unwrap();

    let oracle = Arc::new(MantleProviderOracle::new(client.clone(), 1024));
    let mantle_provider = OracleL2ChainProvider::new(oracle.clone());
    let trie_db = TrieDB::new(
        prev_block.header.state_root,
        Header {
            parent_hash: prev_block.header.parent_hash,
            ommers_hash: prev_block.header.uncles_hash,
            beneficiary: prev_block.header.miner,
            state_root: prev_block.header.state_root,
            transactions_root: prev_block.header.transactions_root,
            receipts_root: prev_block.header.receipts_root,
            logs_bloom: prev_block.header.logs_bloom,
            difficulty: prev_block.header.difficulty,
            number: prev_block.header.number,
            gas_limit: prev_block.header.gas_limit,
            gas_used: prev_block.header.gas_used,
            timestamp: prev_block.header.timestamp,
            extra_data: prev_block.header.extra_data,
            mix_hash: prev_block.header.mix_hash.unwrap_or_default(),
            nonce: prev_block.header.nonce.unwrap_or_default(),
            base_fee_per_gas: prev_block.header.base_fee_per_gas,
            withdrawals_root: prev_block.header.withdrawals_root,
            blob_gas_used: prev_block.header.blob_gas_used,
            excess_blob_gas: prev_block.header.excess_blob_gas,
            parent_beacon_block_root: prev_block.header.parent_beacon_block_root,
            requests_hash: prev_block.header.requests_root,
        }
            .seal_slow(),
        mantle_provider.clone(),
        mantle_provider.clone(),
    );

    let mut state = State::builder()
        .with_database(trie_db)
        .with_bundle_update()
        .build();
    let mut evm = EvmBuilder::default()
        .with_db(&mut state)
        .with_spec_id(SpecId::SHANGHAI)
        .modify_cfg_env(|c| {
            c.chain_id = chain_id;
        })
        .modify_block_env(|b| {
            b.number = U256::from(block.header.number);
            b.timestamp = U256::from(block.header.timestamp);
            b.difficulty = U256::ZERO;
            // b.base_fee_per_gas = U256::from(block.header.base_fee_per_gas);
            b.coinbase = SEQUENCER_FEE_VAULT_ADDRESS;
            b.basefee = U256::from(block.header.base_fee_per_gas.unwrap());
            b.gas_limit = U256::from(block.header.gas_limit);
        })
        .optimism()
        // .append_handler_register(inspector_handle_register)
        .build();

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    let start = Instant::now();

    for tx in block.transactions.as_transactions().unwrap() {
        let tx_number = tx.inner.transaction_index.unwrap();
        // if tx_number != 1 {
        //     continue;
        // }
        println!("current tx_number: {:?}", tx_number);
        let tx_hash = tx.inner.hash;
        let raw_tx = client
            .client()
            .request::<&[B256; 1], Bytes>("debug_getRawTransaction", &[tx_hash])
            .await
            .map_err(|e| anyhow!("Failed to fetch raw transaction: {e}"))
            .unwrap();
        let op_tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref())
            .map_err(|e| anyhow!("Failed to decode EIP-2718 transaction: {e}"))
            .unwrap();
        let env = prepare_tx_env(&op_tx, raw_tx.as_ref()).unwrap();
        evm = evm.modify().with_tx_env(env).build();
        println!(
            "--------------- {:?}: {:?}({:?}) ------------------",
            tx_number,
            tx_hash,
            op_tx.tx_type()
        );
        if let OpTxEnvelope::Deposit(deposit) = &op_tx {
            evm.db_mut().load_cache_account(deposit.from).unwrap();
        }

        let result = evm
            .transact_commit()
            .map_err(|e| anyhow!("Failed to transact: {e}"))
            .unwrap();
        let gas_used = result.gas_used();
        let expected_gas_used = client
            .get_transaction_receipt(tx_hash)
            .await
            .unwrap()
            .map(|receipt| receipt.inner.gas_used)
            .unwrap();
        print!("Expected gas used: {:?}, ", expected_gas_used);
        print!("Actual gas used: {:?} ", gas_used);
        if expected_gas_used == gas_used.as_u128() {
            println!("--- passed✅");
        } else {
            println!("--- failed❌");
        }
        record
            .lock()
            .unwrap()
            .add(expected_gas_used == gas_used.as_u128());
    }
    let elapsed = start.elapsed();
    println!(
        "Finished execution. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );
    record.lock().unwrap().print();
    drop(evm);
    state.merge_transitions(BundleRetention::Reverts);
    let bundle = state.take_bundle();
    let state_root = state
        .database
        .state_root(&bundle)
        .expect("Failed to compute state root");
    if block.header.state_root != state_root {
        println!("State root mismatch: {:?}(block) != {:?}(revm)", block.header.state_root,
                 state_root);
    } else {
        println!("State root matches: {:?}", state_root);
    }
    Ok(())
}

/// Prepares a [TxEnv] with the given [OpTxEnvelope].
///
/// ## Takes
/// - `transaction`: The transaction to prepare the environment for.
/// - `env`: The transaction environment to prepare.
///
/// ## Returns
/// - `Ok(())` if the environment was successfully prepared.
/// - `Err(_)` if an error occurred while preparing the environment.
pub fn prepare_tx_env(transaction: &OpTxEnvelope, encoded_transaction: &[u8]) -> Result<TxEnv> {
    let mut env = TxEnv::default();
    match transaction {
        OpTxEnvelope::Legacy(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.gas_price);
            env.gas_priority_fee = None;
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = tx.chain_id;
            env.nonce = Some(tx.nonce);
            env.access_list.clear();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Eip2930(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.gas_price);
            env.gas_priority_fee = None;
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = Some(tx.chain_id);
            env.nonce = Some(tx.nonce);
            env.access_list = tx.access_list.to_vec();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Eip1559(signed_tx) => {
            let tx = signed_tx.tx();
            env.caller = signed_tx
                .recover_signer()
                .map_err(|e| anyhow!("Failed to recover signer: {e}"))?;
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::from(tx.max_fee_per_gas);
            env.gas_priority_fee = Some(U256::from(tx.max_priority_fee_per_gas));
            env.transact_to = match tx.to {
                TxKind::Call(to) => TransactTo::Call(to),
                TxKind::Create => TransactTo::Create,
            };
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = Some(tx.chain_id);
            env.nonce = Some(tx.nonce);
            env.access_list = tx.access_list.to_vec();
            env.blob_hashes.clear();
            env.max_fee_per_blob_gas.take();
            env.optimism = OptimismFields {
                source_hash: None,
                mint: None,
                is_system_transaction: Some(false),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: None,
                eth_tx_value: None,
            };
            Ok(env)
        }
        OpTxEnvelope::Deposit(tx) => {
            println!("Deposit transaction: {:?}", tx);
            env.caller = tx.from;
            env.access_list.clear();
            env.gas_limit = tx.gas_limit;
            env.gas_price = U256::ZERO;
            env.gas_priority_fee = None;
            match tx.to {
                TxKind::Call(to) => env.transact_to = TransactTo::Call(to),
                TxKind::Create => env.transact_to = TransactTo::Create,
            }
            env.value = tx.value;
            env.data = tx.input.clone();
            env.chain_id = None;
            env.nonce = None;
            env.optimism = OptimismFields {
                source_hash: Some(tx.source_hash),
                mint: tx.mint,
                is_system_transaction: Some(tx.is_system_transaction),
                enveloped_tx: Some(encoded_transaction.to_vec().into()),
                eth_value: tx.eth_value,
                eth_tx_value: tx.eth_value,
            };
            Ok(env)
        }
        _ => Err(anyhow!(
            "Unsupported transaction type: {:?}",
            transaction.tx_type() as u8
        )),
    }
}
