//! Contains the `[L1BlockInfo]` type and its implementation.
use crate::{
    constants::{
        GAS_ORACLE_CONTRACT, L1_BASE_FEE_SLOT, L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT, L1_SCALAR_SLOT,
        NON_ZERO_BYTE_COST, TOKEN_RATIO_SLOT, ZERO_BYTE_COST,
    },
    OpSpecId,
};
use core::ops::Mul;
use revm::{database_interface::Database, primitives::hardfork::SpecId, primitives::U256};

/// L1 block info
///
/// We can extract L1 epoch data from each L2 block, by looking at the `setL1BlockValues`
/// transaction data. This data is then used to calculate the L1 cost of a transaction.
///
/// Here is the format of the `setL1BlockValues` transaction data:
///
/// setL1BlockValues(uint64 _number, uint64 _timestamp, uint256 _basefee, bytes32 _hash,
/// uint64 _sequenceNumber, bytes32 _batcherHash, uint256 _l1FeeOverhead, uint256 _l1FeeScalar)
///
/// For now, we only care about the fields necessary for L1 cost calculation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct L1BlockInfo {
    /// The L2 block number. If not same as the one in the context,
    /// L1BlockInfo is not valid and will be reloaded from the database.
    pub l2_block: Option<U256>,
    /// The base fee of the L1 origin block.
    pub l1_base_fee: U256,
    /// The current L1 fee overhead. None if Ecotone is activated.
    pub l1_fee_overhead: Option<U256>,
    /// The current L1 fee scalar.
    pub l1_base_fee_scalar: U256,
    /// The current token ratio.
    pub token_ratio: Option<U256>,
    /// Last calculated l1 fee cost. Uses as a cache between validation and pre execution stages.
    pub tx_l1_cost: Option<U256>,
}

impl L1BlockInfo {
    /// Try to fetch the L1 block info from the database, post-Jovian.
    fn try_fetch_jovian<DB: Database>(&mut self, db: &mut DB) -> Result<(), DB::Error> {
        let da_footprint_gas_scalar_slot = db
            .storage(L1_BLOCK_CONTRACT, DA_FOOTPRINT_GAS_SCALAR_SLOT)?
            .to_be_bytes::<32>();

        // Extract the first 2 bytes directly as a u16 in big-endian format
        let bytes = [
            da_footprint_gas_scalar_slot[DA_FOOTPRINT_GAS_SCALAR_OFFSET],
            da_footprint_gas_scalar_slot[DA_FOOTPRINT_GAS_SCALAR_OFFSET + 1],
        ];
        self.da_footprint_gas_scalar = Some(u16::from_be_bytes(bytes));

        Ok(())
    }

    /// Try to fetch the L1 block info from the database, post-Isthmus.
    fn try_fetch_isthmus<DB: Database>(&mut self, db: &mut DB) -> Result<(), DB::Error> {
        // Post-isthmus L1 block info
        let operator_fee_scalars = db
            .storage(L1_BLOCK_CONTRACT, OPERATOR_FEE_SCALARS_SLOT)?
            .to_be_bytes::<32>();

        // The `operator_fee_scalar` is stored as a big endian u32 at
        // OPERATOR_FEE_SCALAR_OFFSET.
        self.operator_fee_scalar = Some(U256::from_be_slice(
            operator_fee_scalars[OPERATOR_FEE_SCALAR_OFFSET..OPERATOR_FEE_SCALAR_OFFSET + 4]
                .as_ref(),
        ));
        // The `operator_fee_constant` is stored as a big endian u64 at
        // OPERATOR_FEE_CONSTANT_OFFSET.
        self.operator_fee_constant = Some(U256::from_be_slice(
            operator_fee_scalars[OPERATOR_FEE_CONSTANT_OFFSET..OPERATOR_FEE_CONSTANT_OFFSET + 8]
                .as_ref(),
        ));

        Ok(())
    }

    /// Try to fetch the L1 block info from the database, post-Ecotone.
    fn try_fetch_ecotone<DB: Database>(&mut self, db: &mut DB) -> Result<(), DB::Error> {
        self.l1_blob_base_fee = Some(db.storage(L1_BLOCK_CONTRACT, ECOTONE_L1_BLOB_BASE_FEE_SLOT)?);

        let l1_fee_scalars = db
            .storage(L1_BLOCK_CONTRACT, ECOTONE_L1_FEE_SCALARS_SLOT)?
            .to_be_bytes::<32>();

        self.l1_base_fee_scalar = U256::from_be_slice(
            l1_fee_scalars[BASE_FEE_SCALAR_OFFSET..BASE_FEE_SCALAR_OFFSET + 4].as_ref(),
        );

        let l1_blob_base_fee = U256::from_be_slice(
            l1_fee_scalars[BLOB_BASE_FEE_SCALAR_OFFSET..BLOB_BASE_FEE_SCALAR_OFFSET + 4].as_ref(),
        );
        self.l1_blob_base_fee_scalar = Some(l1_blob_base_fee);

        // Check if the L1 fee scalars are empty. If so, we use the Bedrock cost function.
        // The L1 fee overhead is only necessary if `empty_ecotone_scalars` is true, as it was deprecated in Ecotone.
        self.empty_ecotone_scalars = l1_blob_base_fee.is_zero()
            && l1_fee_scalars[BASE_FEE_SCALAR_OFFSET..BLOB_BASE_FEE_SCALAR_OFFSET + 4]
                == EMPTY_SCALARS;
        self.l1_fee_overhead = self
            .empty_ecotone_scalars
            .then(|| db.storage(L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT))
            .transpose()?;

        Ok(())
    }

    /// Try to fetch the L1 block info from the database.
    pub fn try_fetch<DB: Database>(
        db: &mut DB,
        l2_block: U256,
        spec_id: OpSpecId,
    ) -> Result<L1BlockInfo, DB::Error> {
        // Ensure the L1 Block account is loaded into the cache after Ecotone. With EIP-4788, it is no longer the case
        // that the L1 block account is loaded into the cache prior to the first inquiry for the L1 block info.
        if spec_id.into_eth_spec().is_enabled_in(SpecId::CANCUN) {
            let _ = db.basic(L1_BLOCK_CONTRACT)?;
        }

        let _ = db.basic(GAS_ORACLE_CONTRACT)?;
        let l1_base_fee = db.storage(L1_BLOCK_CONTRACT, L1_BASE_FEE_SLOT)?;
        let token_ratio = db.storage(GAS_ORACLE_CONTRACT, TOKEN_RATIO_SLOT)?;

        let l1_fee_overhead = db.storage(L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT)?;
        let l1_fee_scalar = db.storage(L1_BLOCK_CONTRACT, L1_SCALAR_SLOT)?;

        Ok(L1BlockInfo {
            l2_block,
            l1_base_fee,
            l1_fee_overhead: Some(l1_fee_overhead),
            l1_base_fee_scalar: l1_fee_scalar,
            token_ratio: Some(token_ratio),
            ..Default::default()
        })
    }

    /// Calculate the data gas for posting the transaction on L1. Calldata costs 16 gas per byte
    /// after compression.
    ///
    /// Prior to fjord, calldata costs 16 gas per non-zero byte and 4 gas per zero byte.
    ///
    /// Prior to regolith, an extra 68 non-zero bytes were included in the rollup data costs to
    /// account for the empty signature.
    pub fn data_gas(&self, input: &[u8], spec_id: OpSpecId) -> U256 {
        let mut rollup_data_gas_cost = U256::from(input.iter().fold(0, |acc, byte| {
            acc + if *byte == 0x00 {
                ZERO_BYTE_COST
            } else {
                NON_ZERO_BYTE_COST
            }
        }));

        // Prior to regolith, an extra 68 non zero bytes were included in the rollup data costs.
        if !spec_id.is_enabled_in(OpSpecId::REGOLITH) {
            tokens_in_transaction_data += 68 * NON_ZERO_BYTE_MULTIPLIER_ISTANBUL;
        }

        U256::from(tokens_in_transaction_data.saturating_mul(STANDARD_TOKEN_COST))
    }

    /// Clears the cached L1 cost of the transaction.
    pub fn clear_tx_l1_cost(&mut self) {
        self.tx_l1_cost = None;
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, depending on the [OpSpecId] passed.
    pub fn calculate_tx_l1_cost(&mut self, input: &[u8], spec_id: OpSpecId) -> U256 {
        if let Some(tx_l1_cost) = self.tx_l1_cost {
            return tx_l1_cost;
        }
        // If the input is a deposit transaction or empty, the default value is zero.
        let tx_l1_cost = if input.is_empty() || input.first() == Some(&0x7E) {
            return U256::ZERO;
        } else {
            self.calculate_tx_l1_cost_bedrock(input, spec_id)
        };

        self.tx_l1_cost = Some(tx_l1_cost);
        tx_l1_cost
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, pre-Ecotone.
    fn calculate_tx_l1_cost_bedrock(&self, input: &[u8], spec_id: OpSpecId) -> U256 {
        let rollup_data_gas_cost = self.data_gas(input, spec_id);
        rollup_data_gas_cost
            .saturating_add(self.l1_fee_overhead.unwrap_or_default())
            .saturating_mul(self.l1_base_fee)
            .saturating_mul(self.l1_base_fee_scalar)
            .saturating_mul(self.get_token_ratio())
            .wrapping_div(U256::from(1_000_000))
    }

    /// Get the token ratio. If the token ratio is not set, return 1.
    pub fn get_token_ratio(&self) -> U256 {
        self.token_ratio.unwrap_or(U256::from(1))
    }

    /// Reset the l2_block to u64::MAX.
    pub fn reset_l2_block(&mut self) {
        self.l2_block = u64::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::primitives::{bytes, hex};

    #[test]
    fn test_data_gas_non_zero_bytes() {
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000_000),
            l1_fee_overhead: Some(U256::from(1_000_000)),
            l1_base_fee_scalar: U256::from(1_000_000),
            token_ratio: Some(U256::from(1_000_000)),
            ..Default::default()
        };

        // 0xFACADE = 6 nibbles = 3 bytes
        // 0xFACADE = 1111 1010 . 1100 1010 . 1101 1110

        // Pre-regolith (ie bedrock) has an extra 68 non-zero bytes
        // gas cost = 3 non-zero bytes * NON_ZERO_BYTE_COST + NON_ZERO_BYTE_COST * 68
        // gas cost = 3 * 16 + 68 * 16 = 1136
        let input = bytes!("FACADE");
        let bedrock_data_gas = l1_block_info.data_gas(&input, OpSpecId::BEDROCK);
        assert_eq!(bedrock_data_gas, U256::from(1136));

        // Regolith has no added 68 non zero bytes
        // gas cost = 3 * 16 = 48
        let regolith_data_gas = l1_block_info.data_gas(&input, OpSpecId::REGOLITH);
        assert_eq!(regolith_data_gas, U256::from(48));
    }

    #[test]
    fn test_data_gas_zero_bytes() {
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000_000),
            l1_fee_overhead: Some(U256::from(1_000_000)),
            l1_base_fee_scalar: U256::from(1_000_000),
            token_ratio: Some(U256::from(1_000_000)),
            ..Default::default()
        };

        // 0xFA00CA00DE = 10 nibbles = 5 bytes
        // 0xFA00CA00DE = 1111 1010 . 0000 0000 . 1100 1010 . 0000 0000 . 1101 1110

        // Pre-regolith (ie bedrock) has an extra 68 non-zero bytes
        // gas cost = 3 non-zero * NON_ZERO_BYTE_COST + 2 * ZERO_BYTE_COST + NON_ZERO_BYTE_COST * 68
        // gas cost = 3 * 16 + 2 * 4 + 68 * 16 = 1144
        let input = bytes!("FA00CA00DE");
        let bedrock_data_gas = l1_block_info.data_gas(&input, OpSpecId::BEDROCK);
        assert_eq!(bedrock_data_gas, U256::from(1144));

        // Regolith has no added 68 non zero bytes
        // gas cost = 3 * 16 + 2 * 4 = 56
        let regolith_data_gas = l1_block_info.data_gas(&input, OpSpecId::REGOLITH);
        assert_eq!(regolith_data_gas, U256::from(56));
    }

    #[test]
    fn test_calculate_tx_l1_cost() {
        let mut l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000),
            l1_fee_overhead: Some(U256::from(1_000)),
            l1_base_fee_scalar: U256::from(1_000),
            token_ratio: Some(U256::from(1_000)),
            ..Default::default()
        };

        let input = bytes!("FACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, OpSpecId::REGOLITH);
        assert_eq!(gas_cost, U256::from(1_048_000));
        l1_block_info.clear_tx_l1_cost();

        // Zero rollup data gas cost should result in zero
        let input = bytes!("");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, OpSpecId::REGOLITH);
        assert_eq!(gas_cost, U256::ZERO);
        l1_block_info.clear_tx_l1_cost();
    }

    #[test]
    fn calculate_tx_l1_cost_mantle_eip1559() {
        // rig
        //
        // <https://mantlescan.xyz/block/70683492>
        //
        // The token ratio changed at:
        // 70683076
        // 70683686 (70683492 is in between)
        // <https://mantlescan.xyz/tx/0xe1c72a781f15b0c23104101d52cc7562b520f7c62c9fa2a2269d9cadc8718c0e#eventlog>
        //
        // decoded from
        let mut l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from_be_bytes(hex!(
                "00000000000000000000000000000000000000000000000000000001d04db9ad"
            )), // 7,789,722,029
            l1_fee_overhead: Some(U256::from_be_bytes(hex!(
                "00000000000000000000000000000000000000000000000000000000000000bc"
            ))), // 188
            l1_base_fee_scalar: U256::from_be_bytes(hex!(
                "0000000000000000000000000000000000000000000000000000000000002710"
            )), // 10,000
            token_ratio: Some(U256::from(4368)),
            ..Default::default()
        };

        // second tx in Mantle block 70683492
        // <https://mantlescan.xyz/tx/0xa061114290fbe3c06550e61d5c9cb39c575bad277f3c6a2459446b90b2b02577>
        const TX: &[u8] = &hex!("02f901bf8213888202a584015752a084015752a084b48675bd94d9f4e85489adcd0baf0cd63b4231c6af58c2674589056bc75e2d63100000b9014483bd37f900000001cda86a272531e8640cd7f1a92c01839911b90bb009056bc75e2d63100000074dee7563cd80200147ae0001ac041df48df9791b0654f1dbbf2cc8450c5f2e9d0000000199550aaf158915c17ee0e0f81db48e4c7454b10400000001070202080004010103b24db100060000010200020600000302000006010004050102060001060700ff000000000000000000000000000000000000000000000000262255f4770aebe2d0c8b97a46287dcecc2a0aff78c1b0c915c4faa5fffa6cabf0219da63d7f4cb81bae52e2b8e401de1429b7ca94bb0abbf133ae34a125af1a4704044501fe12ca9567ef1550e430e8201eba5cc46d216ce6dc03f6a759e8e766e956ae8a6a1ed01989ff1c5ac6361c34cad9d7d0015ab4deaddeaddeaddeaddeaddeaddeaddeaddead11110000000000000000000000000000000000000000c001a008570dac13b3b52af488672a168d0f0ed4fd6da12d431c30a7326fd6f03dbe81a051ebad75b430e05b4d33a84c581c3cfd28820abdef2004234be4d8b7abe06f74");

        // l1 gas used for tx and l1 fee for tx, from Mantle block scanner
        // <https://mantlescan.xyz/tx/0xa061114290fbe3c06550e61d5c9cb39c575bad277f3c6a2459446b90b2b02577>
        //
        let expected_l1_gas_used = U256::from(6564);
        let expected_l1_fee = U256::from_be_bytes(hex!(
            "0000000000000000000000000000000000000000000000000007ef4bec40587e" // 223343420220019 wei
        ));

        // test
        // TIPS: the Bedrock's l1GasUsed added the overhead, so we need to add it
        // <https://github.com/ethereum-optimism/op-geth/blob/v1.101411.0/core/types/rollup_cost.go#L206>
        let gas_used = l1_block_info.data_gas(TX, OpSpecId::REGOLITH)
            + l1_block_info.l1_fee_overhead.unwrap_or_default();
        assert_eq!(gas_used, expected_l1_gas_used);

        let l1_fee = l1_block_info.calculate_tx_l1_cost(TX, OpSpecId::REGOLITH);
        assert_eq!(l1_fee, expected_l1_fee)
    }

    #[test]
    fn calculate_tx_l1_cost_mantle_legacy() {
        // rig
        //
        // <https://mantlescan.xyz/block/70718078>
        //
        // decoded from
        let mut l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from_be_bytes(hex!(
                "0000000000000000000000000000000000000000000000000000000168d7ab30"
            )),
            l1_fee_overhead: Some(U256::from_be_bytes(hex!(
                "00000000000000000000000000000000000000000000000000000000000000bc"
            ))), // 188
            l1_base_fee_scalar: U256::from_be_bytes(hex!(
                "0000000000000000000000000000000000000000000000000000000000002710"
            )), // 10,000
            token_ratio: Some(U256::from(4359)),
            ..Default::default()
        };

        // seventh tx in Mantle block 70718078
        // <https://mantlescan.xyz/tx/0x27e8441109b10bc4fa9ceceda6ffbebea47e8d38e3972939435c23dfa70df820>
        const TX: &[u8] = &hex!("f8718301b78e8401312d008410d91858946b80e191f678a8378e1a3009eaf027b7515e88eb87282b83459182d080822733a0f820683c02811f2950e72c1a4c82c41bc31fcb9348c58a14bedbc4025e41bc5fa054896a3ee0dd03f8d29109c5ae6192f19338030ea045e208aeaf24cb11af6a6b");

        // l1 gas used for tx and l1 fee for tx, from Mantle block scanner
        // <https://mantlescan.xyz/tx/0x27e8441109b10bc4fa9ceceda6ffbebea47e8d38e3972939435c23dfa70df820>
        let expected_l1_gas_used = U256::from(2016);
        let expected_l1_fee = U256::from_be_bytes(hex!(
            "0000000000000000000000000000000000000000000000000001e3dad743bf42" // 00053200403062765 wei
        ));

        // test
        let gas_used = l1_block_info.data_gas(TX, OpSpecId::REGOLITH)
            + l1_block_info.l1_fee_overhead.unwrap_or_default();
        assert_eq!(gas_used, expected_l1_gas_used);

        let l1_fee = l1_block_info.calculate_tx_l1_cost(TX, OpSpecId::REGOLITH);
        assert_eq!(l1_fee, expected_l1_fee)
    }

    #[test]
    fn test_reset_l2_block() {
        let mut l1_block_info = L1BlockInfo {
            l2_block: 1,
            ..Default::default()
        };
        l1_block_info.reset_l2_block();
        assert_eq!(l1_block_info.l2_block, u64::MAX);
    }
}
