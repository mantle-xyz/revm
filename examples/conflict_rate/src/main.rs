#![cfg_attr(not(test), warn(unused_crate_dependencies))]
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::Bytes;
use anyhow::{Result, anyhow};
use dotenv::dotenv;
use ethers_core::types::H256;
use ethers_providers::Middleware;
use ethers_providers::{Http, Provider};
use op_alloy_consensus::OpTxEnvelope;
use revm::Evm;
use revm::db::{CacheDB, EthersDB};
use revm::inspectors::GasInspector;
use revm::interpreter::opcode;
use revm::primitives::{
    Address, OptimismFields, ResultAndState, SpecId, TransactTo, TxEnv, TxKind, U256,
};
use revm::{
    EvmContext, Inspector,
    interpreter::{CallInputs, CallOutcome, CreateInputs, CreateOutcome, Interpreter},
    primitives::db::Database,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

struct ConflictAnalyzer {
    mnt_transfers: HashMap<Address, Vec<u64>>, // source address -> transaction number list
    storage_accesses: HashMap<(Address, U256), Vec<(u64, AccessType)>>, // (contract address, slot) -> [(transaction number, access type)]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessType {
    Read,
    Write,
}

impl ConflictAnalyzer {
    fn new() -> Self {
        Self {
            mnt_transfers: HashMap::new(),
            storage_accesses: HashMap::new(),
        }
    }

    fn record_mnt_transfer(&mut self, from: Address, tx_number: u64) {
        self.mnt_transfers.entry(from).or_default().push(tx_number);
    }

    fn record_storage_access(
        &mut self,
        contract: Address,
        slot: U256,
        tx_number: u64,
        access_type: AccessType,
    ) {
        self.storage_accesses
            .entry((contract, slot))
            .or_default()
            .push((tx_number, access_type));
    }

    fn analyze_conflicts(&self) -> Vec<Conflict> {
        let mut conflicts = Vec::new();

        // 1. Multiple MNT transfers from the same source address
        for (address, txs) in &self.mnt_transfers {
            if txs.len() > 1 {
                conflicts.push(Conflict {
                    conflict_type: ConflictType::MultipleMntTransfersFromSameSource,
                    address: *address,
                    transactions: txs.clone(),
                    storage_slot: None,
                });
            }
        }

        // 2. Analyze storage slot conflicts
        for ((contract, slot), accesses) in &self.storage_accesses {
            // First group by transaction number and merge multiple accesses from the same transaction
            let mut tx_accesses: HashMap<u64, AccessType> = HashMap::new();
            
            for (tx_num, access_type) in accesses {
                // If the same transaction has both read and write, record it as write
                tx_accesses.entry(*tx_num)
                    .and_modify(|e| {
                        if *access_type == AccessType::Write {
                            *e = AccessType::Write
                        }
                    })
                    .or_insert(*access_type);
            }

            // Check if there is a write operation
            let has_write = tx_accesses.values().any(|at| *at == AccessType::Write);
            
            // If there is a write operation and involves multiple different transactions, there is a conflict
            if has_write && tx_accesses.len() > 1 {
                let conflict_txs: Vec<u64> = tx_accesses.keys().cloned().collect();
                
                conflicts.push(Conflict {
                    conflict_type: ConflictType::StorageSlotConflict,
                    address: *contract,
                    transactions: conflict_txs,
                    storage_slot: Some(*slot),
                });
            }
        }

        conflicts
    }

    fn print_analysis(&self, total_txs: usize) -> usize {
        let conflicts = self.analyze_conflicts();

        // Get the number of affected transactions
        let affected_txs: HashSet<u64> = conflicts
            .iter()
            .flat_map(|c| c.transactions.clone())
            .collect();

        let affected_count = affected_txs.len();

        // Calculate the conflict rate
        let conflict_ratio = if total_txs > 0 {
            affected_count as f64 / total_txs as f64
        } else {
            0.0
        };

        println!(
            "Conflict rate: {:.6} ({} / {})",
            conflict_ratio, affected_count, total_txs
        );

        // Return the number of conflicted transactions
        affected_count
    }
}

#[derive(Debug)]
enum ConflictType {
    MultipleMntTransfersFromSameSource,
    StorageSlotConflict,
}

#[derive(Debug)]
struct Conflict {
    conflict_type: ConflictType,
    address: Address,
    transactions: Vec<u64>,
    storage_slot: Option<U256>,
}

macro_rules! local_fill {
    ($left:expr, $right:expr, $fun:expr) => {
        if let Some(right) = $right {
            $left = $fun(right.0)
        }
    };
    ($left:expr, $right:expr) => {
        if let Some(right) = $right {
            $left = Address::from(right.as_fixed_bytes())
        }
    };
}

struct GlobalStats {
    total_blocks: usize,
    invalid_blocks: usize,
    total_txs: usize,
    conflicted_txs: usize,
    same_source_conflicts: usize,
    storage_slot_conflicts: usize,
    start_time: Instant,
}

impl GlobalStats {
    fn new() -> Self {
        Self {
            total_blocks: 0,
            invalid_blocks: 0,
            total_txs: 0,
            conflicted_txs: 0,
            same_source_conflicts: 0,
            storage_slot_conflicts: 0,
            start_time: Instant::now(),
        }
    }
    
    fn add_block_stats(&mut self, total_block_txs: usize, conflicted_block_txs: usize, conflicts: &[Conflict]) {
        self.total_blocks += 1;
        self.total_txs += total_block_txs;
        self.conflicted_txs += conflicted_block_txs;
        
        // Count different types of conflicts
        for conflict in conflicts {
            match conflict.conflict_type {
                ConflictType::MultipleMntTransfersFromSameSource => {
                    self.same_source_conflicts += conflict.transactions.len();
                }
                ConflictType::StorageSlotConflict => {
                    self.storage_slot_conflicts += conflict.transactions.len();
                }
            }
        }
    }
    
    fn record_invalid_block(&mut self) {
        self.invalid_blocks += 1;
    }
    
    fn print_final_stats(&self) {
        let elapsed = self.start_time.elapsed();
        let dependency_ratio = if self.total_txs > 0 {
            (self.conflicted_txs as f64 / self.total_txs as f64) * 100.0
        } else {
            0.0
        };
        
        println!("\nAnalysis Results:");
        println!("  Time taken: {:.2} seconds", elapsed.as_secs_f64());
        println!("  Total blocks: {}", self.total_blocks);
        println!("  Invalid blocks: {}", self.invalid_blocks);
        println!("  Total transactions: {}", self.total_txs);
        println!("  Dependent transactions: {}", self.conflicted_txs);
        println!("  Dependency ratio: {:.2}%", dependency_ratio);
        println!("  Conflict counts:");
        println!("    same-source: {}", self.same_source_conflicts);
        println!("    contract-slot-conflict: {}", self.storage_slot_conflicts);
    }
}

// Storage read tracker
#[derive(Default, Clone)]
struct StorageReadInspector {
    tx_number: u64,
    reads: RefCell<HashMap<(Address, U256), ()>>, // Use HashMap to record existence only, avoiding duplicates
    gas_inspector: GasInspector,
}

impl StorageReadInspector {
    fn new(tx_number: u64) -> Self {
        Self {
            tx_number,
            reads: RefCell::new(HashMap::new()),
            gas_inspector: GasInspector::default(),
        }
    }

    // Get read storage slots
    fn get_read_slots(&self) -> Vec<(Address, U256)> {
        self.reads.borrow().keys().cloned().collect()
    }
}

impl<DB: Database> Inspector<DB> for StorageReadInspector {
    // Initialize interpreter
    fn initialize_interp(&mut self, interp: &mut Interpreter, context: &mut EvmContext<DB>) {
        self.gas_inspector.initialize_interp(interp, context);
    }

    // Capture sload operation
    fn step(&mut self, interp: &mut Interpreter, context: &mut EvmContext<DB>) {
        self.gas_inspector.step(interp, context);

        // Check if the current opcode is SLOAD
        if interp.current_opcode() == opcode::SLOAD {
            // SLOAD will pop the storage slot index from the stack
            if let Ok(slot) = interp.stack.peek(0) {
                let address = interp.contract.target_address;
                self.reads.borrow_mut().insert((address, slot), ());
            }
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter, context: &mut EvmContext<DB>) {
        self.gas_inspector.step_end(interp, context);
    }

    fn call_end(
        &mut self,
        context: &mut EvmContext<DB>,
        inputs: &CallInputs,
        outcome: CallOutcome,
    ) -> CallOutcome {
        self.gas_inspector.call_end(context, inputs, outcome)
    }

    fn create_end(
        &mut self,
        context: &mut EvmContext<DB>,
        inputs: &CreateInputs,
        outcome: CreateOutcome,
    ) -> CreateOutcome {
        self.gas_inspector.create_end(context, inputs, outcome)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start = 77075444;
    let mut global_stats = GlobalStats::new();
    
    for block_number in start..start + 2 {
        match range(block_number).await {
            Ok((block_txs, block_conflicts, conflicts)) => {
                global_stats.add_block_stats(block_txs, block_conflicts, &conflicts);
            }
            Err(_) => {
                global_stats.record_invalid_block();
            }
        }
    }
    
    global_stats.print_final_stats();
    Ok(())
}

async fn range(block_number: u64) -> anyhow::Result<(usize, usize, Vec<Conflict>)> {
    dotenv().ok();
    let mantle_url = std::env::var("MANTLE_URL").unwrap();

    let client = Provider::<Http>::try_from(mantle_url)?;
    let client = Arc::new(client);

    let chain_id: u64 = client.get_chainid().await.unwrap().as_u64();

    let block = match client.get_block_with_txs(block_number).await {
        Ok(Some(block)) => block,
        Ok(None) => anyhow::bail!("Block not found"),
        Err(error) => anyhow::bail!("Error: {:?}", error),
    };
    println!("Fetched block number: {}", block.number.unwrap().0[0]);
    let previous_block_number = block_number - 1;

    let prev_id = previous_block_number.into();

    let state_db = EthersDB::new(client.clone(), Some(prev_id)).expect("panic");
    let mut cache_db = CacheDB::new(state_db);

    let mut evm = Evm::builder()
        .with_db(&mut cache_db)
        .with_external_context(StorageReadInspector::new(0))
        .modify_block_env(|b| {
            if let Some(number) = block.number {
                let nn = number.0[0];
                b.number = U256::from(nn);
            }
            local_fill!(b.coinbase, block.author);
            local_fill!(b.timestamp, Some(block.timestamp), U256::from_limbs);
            local_fill!(b.difficulty, Some(block.difficulty), U256::from_limbs);
            local_fill!(b.gas_limit, Some(block.gas_limit), U256::from_limbs);
            if let Some(base_fee) = block.base_fee_per_gas {
                local_fill!(b.basefee, Some(base_fee), U256::from_limbs);
            }
        })
        .with_spec_id(SpecId::SHANGHAI)
        .modify_cfg_env(|c| {
            c.chain_id = chain_id;
        })
        .optimism()
        .build();

    let txs = block.transactions.len();
    println!("Found {txs} transactions.");

    let start = Instant::now();
    let mut conflict_analyzer = ConflictAnalyzer::new();

    for tx in block.transactions {
        let tx_number = tx.transaction_index.unwrap().0[0];
        let tx_hash = tx.hash;
        let raw_tx = client
            .request::<&[H256; 1], Bytes>("debug_getRawTransaction", &[tx_hash.into()])
            .await
            .map_err(|e| anyhow!("Failed to fetch raw transaction: {e}"))?;
        let op_tx = OpTxEnvelope::decode_2718(&mut raw_tx.as_ref())
            .map_err(|e| anyhow!("Failed to decode EIP-2718 transaction: {e}"))?;
        let env = prepare_tx_env(&op_tx, raw_tx.as_ref())?;

        // value is not zero, it is a MNT transfer
        let from_address = env.caller;
        if !env.value.is_zero() {
            conflict_analyzer.record_mnt_transfer(from_address, tx_number);
        }

        let inspector = StorageReadInspector::new(tx_number);
        evm = evm
            .modify()
            .with_tx_env(env)
            .reset_handler_with_external_context(inspector.clone()) // use clone
            .build();

        let ResultAndState { result, state } = evm
            .transact()
            .map_err(|e| anyhow!("Failed to transact: {e}"))?;

        if result.is_success() {
            for (address, account) in &state {
                if account.is_touched() {
                    for (slot_key, slot) in &account.storage {
                        if slot.is_changed() {
                            // 记录写入操作
                            conflict_analyzer.record_storage_access(
                                *address,
                                *slot_key,
                                tx_number,
                                AccessType::Write,
                            );
                        }
                    }
                }
            }
        }

        let read_slots = inspector.get_read_slots();

        for (address, slot) in read_slots {
            conflict_analyzer.record_storage_access(address, slot, tx_number, AccessType::Read);
        }
    }

    let conflicts = conflict_analyzer.analyze_conflicts();
    let affected_txs = conflicts
        .iter()
        .flat_map(|c| c.transactions.clone())
        .collect::<HashSet<_>>()
        .len();

    let elapsed = start.elapsed();
    println!(
        "Finished execution. Total CPU time: {:.6}s",
        elapsed.as_secs_f64()
    );
    drop(evm);

    Ok((txs, affected_txs, conflicts))
}

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
                eth_tx_value: tx.eth_tx_value,
            };
            Ok(env)
        }
        _ => Err(anyhow!(
            "Unsupported transaction type: {:?}",
            transaction.tx_type() as u8
        )),
    }
}
