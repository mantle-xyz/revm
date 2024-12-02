//! Optimism-specific constants, types, and helpers.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use alloy::network::primitives::BlockTransactions;
use alloy::{
    eips::BlockNumberOrTag,
    network::primitives::BlockTransactionsKind,
    providers::{Provider, ProviderBuilder},
    rpc::types::Header as RpcHeader,
};
use alloy_consensus::Header;
use alloy_primitives::{Bytes, Sealable};
use alloy_rpc_types::Block;
use alloy_rpc_types_engine::PayloadAttributes;
use anyhow::anyhow;
use client::{mantle::OracleL2ChainProvider, oracle::MantleProviderOracle};
use dotenv::dotenv;
use kona_driver::{
    Executor, ExecutorConstructor,
};
use kona_proof::executor::KonaExecutorConstructor;
use op_alloy_genesis::rollup::RollupConfig;
use op_alloy_network::Optimism;
// use op_alloy_rpc_types::Transaction as RpcTransaction;
use op_alloy_rpc_types_engine::OpPayloadAttributes;
use revm::primitives::B256;
use revm::SEQUENCER_FEE_VAULT_ADDRESS;
use std::sync::Arc;
use tracing::Level;

#[tokio::main]
async fn main() {
    // Initialize the logger
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("client_executor=debug".parse().unwrap()),
        )
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|e| anyhow!(e))
        .unwrap();
    // ----------------------------

    dotenv().ok();
    let block_number = 71632023;
    // let block_number = 72357146;
    let mantle_url = std::env::var("MANTLE_URL").unwrap();
    let url = mantle_url.as_str();
    let client = ProviderBuilder::new()
        .network::<Optimism>()
        .on_http(url.parse().unwrap());
    let client = Arc::new(client);

    let prev_block = client
        .get_block_by_number(
            BlockNumberOrTag::from(block_number - 1),
            BlockTransactionsKind::Hashes,
        )
        .await
        .unwrap()
        .ok_or(anyhow!("Block not found"))
        .unwrap();

    let prev_block_header = convert_header(prev_block.header);

    println!("cycle-tracker-report-start: payload-derivation");
    let Block {
        transactions,
        header,
        ..
    } = client
        .get_block_by_number(
            BlockNumberOrTag::from(block_number),
            BlockTransactionsKind::Hashes,
        )
        .await
        .unwrap()
        .ok_or(anyhow!("Block not found"))
        .unwrap();

    let txs = match transactions {
        BlockTransactions::Hashes(transactions) => {
            let mut encoded_transactions = Vec::with_capacity(transactions.len());
            for tx_hash in transactions {
                let tx = client
                    .client()
                    .request::<&[B256; 1], Bytes>("debug_getRawTransaction", &[tx_hash])
                    .await
                    .unwrap();
                encoded_transactions.push(tx);
            }
            encoded_transactions
        }
        _ => {
            anyhow::anyhow!("Only BlockTransactions::Hashes are supported.");
            vec![]
        }
    };

    let attributes = prepare_payload(header.clone(), txs);
    println!("cycle-tracker-report-end: payload-derivation");

    println!("cycle-tracker-start: execution-instantiation");
    let oracle = Arc::new(MantleProviderOracle::new(client.clone(), 1024));

    // let input = std::fs::read(format!("cache-{}.bin", block_number).as_str()).unwrap();
    // let oracle = Arc::new(InMemoryOracle::from_raw_bytes(input));
    let mantle_provider = OracleL2ChainProvider::new(oracle.clone());
    let config = mock_rollup_config();
    let binding = Arc::new(config);
    let executor_constructor = KonaExecutorConstructor::new(
        &binding,
        mantle_provider.clone(),
        mantle_provider.clone(),
        None,
    );
    let mut executor = executor_constructor.new_executor(prev_block_header.seal_slow());
    println!("cycle-tracker-end: execution-instantiation");

    println!("cycle-tracker-report-start: block-execution");
    let new_block_header = executor.execute_payload(attributes.clone()).unwrap();
    println!("new block header: {:?}", new_block_header);
    println!("cycle-tracker-report-end: block-execution");

    let new_block_number = new_block_header.number;
    println!("New block number: {}", new_block_number);

    if convert_header(header.clone()) == *new_block_header {
        println!("🎉🎉🎉🎉Block execution successful🎉🎉🎉🎉");
    } else {
        println!("❌❌❌❌Block execution failed❌❌❌❌");
    }
    println!("cycle-tracker-start: output-root");
    let output_root = executor.compute_output_root().unwrap();
    println!("Output root: {}", output_root);
    println!("cycle-tracker-end: output-root");

    println!("cycle-tracker-start: cache-dump");
    oracle
        .dump_cache_to_binary_file(format!("cache-{}.bin", new_block_number).as_str())
        .unwrap();
    println!("cycle-tracker-end: cache-dump");
}

fn mock_rollup_config() -> RollupConfig {
    RollupConfig {
        l2_chain_id: 5000,
        regolith_time: Some(0),
        shanghai_time: Some(0),
        ..Default::default()
    }
}

fn convert_header(header: RpcHeader) -> Header {
    Header {
        parent_hash: header.parent_hash,
        ommers_hash: header.ommers_hash,
        beneficiary: header.beneficiary,
        state_root: header.state_root,
        transactions_root: header.transactions_root,
        receipts_root: header.receipts_root,
        logs_bloom: header.logs_bloom,
        difficulty: header.difficulty,
        number: header.number,
        gas_limit: header.gas_limit,
        gas_used: header.gas_used,
        timestamp: header.timestamp,
        extra_data: header.extra_data.clone(),
        mix_hash: header.mix_hash,
        nonce: header.nonce,
        base_fee_per_gas: header.base_fee_per_gas,
        withdrawals_root: header.withdrawals_root,
        blob_gas_used: header.blob_gas_used,
        excess_blob_gas: header.excess_blob_gas,
        parent_beacon_block_root: header.parent_beacon_block_root,
        requests_hash: header.requests_hash,
    }
}

pub fn prepare_payload(header: RpcHeader, txs: Vec<Bytes>) -> OpPayloadAttributes {
    OpPayloadAttributes {
        payload_attributes: PayloadAttributes {
            timestamp: header.timestamp,
            prev_randao: header.mix_hash,
            suggested_fee_recipient: SEQUENCER_FEE_VAULT_ADDRESS,
            parent_beacon_block_root: None,
            withdrawals: Some(Vec::default()),
        },
        transactions: Some(txs),
        no_tx_pool: Some(true),
        gas_limit: Some(header.gas_limit),
        base_fee: None,
    }
}
