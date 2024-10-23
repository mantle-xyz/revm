use crate::fast_lz::flz_compress_len;
use core::ops::Mul;
use revm::{
    database_interface::Database,
    primitives::{address, Address, U256},
};

use super::MantleSpecId;

const ZERO_BYTE_COST: u64 = 4;
const NON_ZERO_BYTE_COST: u64 = 16;

/// The two 4-byte Ecotone fee scalar values are packed into the same storage slot as the 8-byte sequence number.
/// Byte offset within the storage slot of the 4-byte baseFeeScalar attribute.
const BASE_FEE_SCALAR_OFFSET: usize = 16;
/// The two 4-byte Ecotone fee scalar values are packed into the same storage slot as the 8-byte sequence number.
/// Byte offset within the storage slot of the 4-byte blobBaseFeeScalar attribute.
const BLOB_BASE_FEE_SCALAR_OFFSET: usize = 20;

const L1_BASE_FEE_SLOT: U256 = U256::from_limbs([1u64, 0, 0, 0]);
const L1_OVERHEAD_SLOT: U256 = U256::from_limbs([5u64, 0, 0, 0]);
const L1_SCALAR_SLOT: U256 = U256::from_limbs([6u64, 0, 0, 0]);

const TOKEN_RATIO_SLOT: U256 = U256::from_limbs([0u64, 0, 0, 0]);

/// [ECOTONE_L1_BLOB_BASE_FEE_SLOT] was added in the Ecotone upgrade and stores the L1 blobBaseFee attribute.
const ECOTONE_L1_BLOB_BASE_FEE_SLOT: U256 = U256::from_limbs([7u64, 0, 0, 0]);

/// As of the ecotone upgrade, this storage slot stores the 32-bit basefeeScalar and blobBaseFeeScalar attributes at
/// offsets [BASE_FEE_SCALAR_OFFSET] and [BLOB_BASE_FEE_SCALAR_OFFSET] respectively.
const ECOTONE_L1_FEE_SCALARS_SLOT: U256 = U256::from_limbs([3u64, 0, 0, 0]);

/// An empty 64-bit set of scalar values.
const EMPTY_SCALARS: [u8; 8] = [0u8; 8];

/// The address of L1 fee recipient.
/// Mantle does not use this address.
// pub const L1_FEE_RECIPIENT: Address = address!("420000000000000000000000000000000000001A");

/// The address of the base fee recipient.
pub const BASE_FEE_RECIPIENT: Address = address!("4200000000000000000000000000000000000019");

/// The address of the L1Block contract.
pub const L1_BLOCK_CONTRACT: Address = address!("4200000000000000000000000000000000000015");

/// The address of the gas oracle contract.
pub const GAS_ORACLE_CONTRACT: Address = address!("420000000000000000000000000000000000000F");

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
    /// The base fee of the L1 origin block.
    pub l1_base_fee: U256,
    /// The current L1 fee overhead. None if Ecotone is activated.
    pub l1_fee_overhead: Option<U256>,
    /// The current L1 fee scalar.
    pub l1_base_fee_scalar: U256,
    /// The current L1 blob base fee. None if Ecotone is not activated, except if `empty_scalars` is `true`.
    pub l1_blob_base_fee: Option<U256>,
    /// The current L1 blob base fee scalar. None if Ecotone is not activated.
    pub l1_blob_base_fee_scalar: Option<U256>,
    /// The current token ratio.
    pub token_ratio: Option<U256>,
    /// True if Ecotone is activated, but the L1 fee scalars have not yet been set.
    pub(crate) empty_scalars: bool,
}

impl L1BlockInfo {
    /// Try to fetch the L1 block info from the database.
    pub fn try_fetch<DB: Database>(
        db: &mut DB,
        spec_id: MantleSpecId,
    ) -> Result<L1BlockInfo, DB::Error> {
        // Ensure the L1 Block account is loaded into the cache after Ecotone. With EIP-4788, it is no longer the case
        // that the L1 block account is loaded into the cache prior to the first inquiry for the L1 block info.
        if spec_id.is_enabled_in(MantleSpecId::CANCUN) {
            let _ = db.basic(L1_BLOCK_CONTRACT)?;
        }
        let _ = db.basic(GAS_ORACLE_CONTRACT)?;

        let l1_base_fee = db.storage(L1_BLOCK_CONTRACT, L1_BASE_FEE_SLOT)?;
        let token_ratio = db.storage(GAS_ORACLE_CONTRACT, TOKEN_RATIO_SLOT)?;

        if !spec_id.is_enabled_in(MantleSpecId::ECOTONE) {
            let l1_fee_overhead = db.storage(L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT)?;
            let l1_fee_scalar = db.storage(L1_BLOCK_CONTRACT, L1_SCALAR_SLOT)?;

            Ok(L1BlockInfo {
                l1_base_fee,
                l1_fee_overhead: Some(l1_fee_overhead),
                l1_base_fee_scalar: l1_fee_scalar,
                token_ratio: Some(token_ratio),
                ..Default::default()
            })
        } else {
            let l1_blob_base_fee = db.storage(L1_BLOCK_CONTRACT, ECOTONE_L1_BLOB_BASE_FEE_SLOT)?;
            let l1_fee_scalars = db
                .storage(L1_BLOCK_CONTRACT, ECOTONE_L1_FEE_SCALARS_SLOT)?
                .to_be_bytes::<32>();

            let l1_base_fee_scalar = U256::from_be_slice(
                l1_fee_scalars[BASE_FEE_SCALAR_OFFSET..BASE_FEE_SCALAR_OFFSET + 4].as_ref(),
            );
            let l1_blob_base_fee_scalar = U256::from_be_slice(
                l1_fee_scalars[BLOB_BASE_FEE_SCALAR_OFFSET..BLOB_BASE_FEE_SCALAR_OFFSET + 4]
                    .as_ref(),
            );

            // Check if the L1 fee scalars are empty. If so, we use the Bedrock cost function. The L1 fee overhead is
            // only necessary if `empty_scalars` is true, as it was deprecated in Ecotone.
            let empty_scalars = l1_blob_base_fee.is_zero()
                && l1_fee_scalars[BASE_FEE_SCALAR_OFFSET..BLOB_BASE_FEE_SCALAR_OFFSET + 4]
                    == EMPTY_SCALARS;
            let l1_fee_overhead = empty_scalars
                .then(|| db.storage(L1_BLOCK_CONTRACT, L1_OVERHEAD_SLOT))
                .transpose()?;

            Ok(L1BlockInfo {
                l1_base_fee,
                l1_base_fee_scalar,
                l1_blob_base_fee: Some(l1_blob_base_fee),
                l1_blob_base_fee_scalar: Some(l1_blob_base_fee_scalar),
                empty_scalars,
                l1_fee_overhead,
                token_ratio: Some(token_ratio),
            })
        }
    }

    /// Calculate the data gas for posting the transaction on L1. Calldata costs 16 gas per byte
    /// after compression.
    ///
    /// Prior to fjord, calldata costs 16 gas per non-zero byte and 4 gas per zero byte.
    ///
    /// Prior to regolith, an extra 68 non-zero bytes were included in the rollup data costs to
    /// account for the empty signature.
    pub fn data_gas(&self, input: &[u8], spec_id: MantleSpecId) -> U256 {
        if spec_id.is_enabled_in(MantleSpecId::FJORD) {
            let estimated_size = self.tx_estimated_size_fjord(input);

            return estimated_size
                .saturating_mul(U256::from(NON_ZERO_BYTE_COST))
                .wrapping_div(U256::from(1_000_000));
        };

        let mut rollup_data_gas_cost = U256::from(input.iter().fold(0, |acc, byte| {
            acc + if *byte == 0x00 {
                ZERO_BYTE_COST
            } else {
                NON_ZERO_BYTE_COST
            }
        }));

        // Prior to regolith, an extra 68 non zero bytes were included in the rollup data costs.
        if !spec_id.is_enabled_in(MantleSpecId::REGOLITH) {
            rollup_data_gas_cost += U256::from(NON_ZERO_BYTE_COST).mul(U256::from(68));
        }

        rollup_data_gas_cost
    }

    // Calculate the estimated compressed transaction size in bytes, scaled by 1e6.
    // This value is computed based on the following formula:
    // max(minTransactionSize, intercept + fastlzCoef*fastlzSize)
    fn tx_estimated_size_fjord(&self, input: &[u8]) -> U256 {
        let fastlz_size = U256::from(flz_compress_len(input));

        fastlz_size
            .saturating_mul(U256::from(836_500))
            .saturating_sub(U256::from(42_585_600))
            .max(U256::from(100_000_000))
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, depending on the [MantleSpecId] passed.
    pub fn calculate_tx_l1_cost(&self, input: &[u8], spec_id: MantleSpecId) -> U256 {
        // If the input is a deposit transaction or empty, the default value is zero.
        if input.is_empty() || input.first() == Some(&0x7F) {
            return U256::ZERO;
        }

        if spec_id.is_enabled_in(MantleSpecId::FJORD) {
            self.calculate_tx_l1_cost_fjord(input)
        } else if spec_id.is_enabled_in(MantleSpecId::ECOTONE) {
            self.calculate_tx_l1_cost_ecotone(input, spec_id)
        } else {
            self.calculate_tx_l1_cost_bedrock(input, spec_id)
        }
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, pre-Ecotone.
    fn calculate_tx_l1_cost_bedrock(&self, input: &[u8], spec_id: MantleSpecId) -> U256 {
        let rollup_data_gas_cost = self.data_gas(input, spec_id);

        rollup_data_gas_cost
            .saturating_add(self.l1_fee_overhead.unwrap_or_default())
            .saturating_mul(self.l1_base_fee)
            .saturating_mul(self.l1_base_fee_scalar)
            .saturating_mul(self.get_token_ratio())
            .wrapping_div(U256::from(1_000_000))
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, post-Ecotone.
    ///
    /// [MantleSpecId::ECOTONE] L1 cost function:
    /// `(calldataGas/16)*(l1BaseFee*16*l1BaseFeeScalar + l1BlobBaseFee*l1BlobBaseFeeScalar)/1e6`
    ///
    /// We divide "calldataGas" by 16 to change from units of calldata gas to "estimated # of bytes when compressed".
    /// Known as "compressedTxSize" in the spec.
    ///
    /// Function is actually computed as follows for better precision under integer arithmetic:
    /// `calldataGas*(l1BaseFee*16*l1BaseFeeScalar + l1BlobBaseFee*l1BlobBaseFeeScalar)/16e6`
    fn calculate_tx_l1_cost_ecotone(&self, input: &[u8], spec_id: MantleSpecId) -> U256 {
        // There is an edgecase where, for the very first Ecotone block (unless it is activated at Genesis), we must
        // use the Bedrock cost function. To determine if this is the case, we can check if the Ecotone parameters are
        // unset.
        if self.empty_scalars {
            return self.calculate_tx_l1_cost_bedrock(input, spec_id);
        }

        let rollup_data_gas_cost = self.data_gas(input, spec_id);
        let l1_fee_scaled = self.calculate_l1_fee_scaled_ecotone();

        l1_fee_scaled
            .saturating_mul(rollup_data_gas_cost)
            .wrapping_div(U256::from(1_000_000 * NON_ZERO_BYTE_COST))
    }

    /// Calculate the gas cost of a transaction based on L1 block data posted on L2, post-Fjord.
    ///
    /// [MantleSpecId::FJORD] L1 cost function:
    /// `estimatedSize*(baseFeeScalar*l1BaseFee*16 + blobFeeScalar*l1BlobBaseFee)/1e12`
    fn calculate_tx_l1_cost_fjord(&self, input: &[u8]) -> U256 {
        let l1_fee_scaled = self.calculate_l1_fee_scaled_ecotone();
        let estimated_size = self.tx_estimated_size_fjord(input);

        estimated_size
            .saturating_mul(l1_fee_scaled)
            .wrapping_div(U256::from(1_000_000_000_000u64))
    }

    // l1BaseFee*16*l1BaseFeeScalar + l1BlobBaseFee*l1BlobBaseFeeScalar
    fn calculate_l1_fee_scaled_ecotone(&self) -> U256 {
        let calldata_cost_per_byte = self
            .l1_base_fee
            .saturating_mul(U256::from(NON_ZERO_BYTE_COST))
            .saturating_mul(self.l1_base_fee_scalar);
        let blob_cost_per_byte = self
            .l1_blob_base_fee
            .unwrap_or_default()
            .saturating_mul(self.l1_blob_base_fee_scalar.unwrap_or_default());

        calldata_cost_per_byte.saturating_add(blob_cost_per_byte)
    }

    pub fn get_token_ratio(&self) -> U256 {
        self.token_ratio.unwrap_or(U256::from(1))
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
        let bedrock_data_gas = l1_block_info.data_gas(&input, MantleSpecId::BEDROCK);
        assert_eq!(bedrock_data_gas, U256::from(1136));

        // Regolith has no added 68 non zero bytes
        // gas cost = 3 * 16 = 48
        let regolith_data_gas = l1_block_info.data_gas(&input, MantleSpecId::REGOLITH);
        assert_eq!(regolith_data_gas, U256::from(48));

        // Fjord has a minimum compressed size of 100 bytes
        // gas cost = 100 * 16 = 1600
        let fjord_data_gas = l1_block_info.data_gas(&input, MantleSpecId::FJORD);
        assert_eq!(fjord_data_gas, U256::from(1600));
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
        let bedrock_data_gas = l1_block_info.data_gas(&input, MantleSpecId::BEDROCK);
        assert_eq!(bedrock_data_gas, U256::from(1144));

        // Regolith has no added 68 non zero bytes
        // gas cost = 3 * 16 + 2 * 4 = 56
        let regolith_data_gas = l1_block_info.data_gas(&input, MantleSpecId::REGOLITH);
        assert_eq!(regolith_data_gas, U256::from(56));

        // Fjord has a minimum compressed size of 100 bytes
        // gas cost = 100 * 16 = 1600
        let fjord_data_gas = l1_block_info.data_gas(&input, MantleSpecId::FJORD);
        assert_eq!(fjord_data_gas, U256::from(1600));
    }

    #[test]
    fn test_calculate_tx_l1_cost() {
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000),
            l1_fee_overhead: Some(U256::from(1_000)),
            l1_base_fee_scalar: U256::from(1_000),
            token_ratio: Some(U256::from(1_000)),
            ..Default::default()
        };

        let input = bytes!("FACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::REGOLITH);
        assert_eq!(gas_cost, U256::from(1_048_000));

        // Zero rollup data gas cost should result in zero
        let input = bytes!("");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::REGOLITH);
        assert_eq!(gas_cost, U256::ZERO);

        // Deposit transactions with the EIP-2718 type of 0x7F should result in zero
        let input = bytes!("7FFACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::REGOLITH);
        assert_eq!(gas_cost, U256::ZERO);
    }

    #[test]
    fn test_calculate_tx_l1_cost_ecotone() {
        let mut l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000),
            l1_base_fee_scalar: U256::from(1_000),
            l1_blob_base_fee: Some(U256::from(1_000)),
            l1_blob_base_fee_scalar: Some(U256::from(1_000)),
            l1_fee_overhead: Some(U256::from(1_000)),
            token_ratio: Some(U256::from(1_000)),
            ..Default::default()
        };

        // calldataGas * (l1BaseFee * 16 * l1BaseFeeScalar + l1BlobBaseFee * l1BlobBaseFeeScalar) / (16 * 1e6)
        // = (16 * 3) * (1000 * 16 * 1000 + 1000 * 1000) / (16 * 1e6)
        // = 51
        let input = bytes!("FACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::ECOTONE);
        assert_eq!(gas_cost, U256::from(51));

        // Zero rollup data gas cost should result in zero
        let input = bytes!("");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::ECOTONE);
        assert_eq!(gas_cost, U256::ZERO);

        // Deposit transactions with the EIP-2718 type of 0x7F should result in zero
        let input = bytes!("7FFACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::ECOTONE);
        assert_eq!(gas_cost, U256::ZERO);

        // If the scalars are empty, the bedrock cost function should be used.
        l1_block_info.empty_scalars = true;
        let input = bytes!("FACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::ECOTONE);
        assert_eq!(gas_cost, U256::from(1_048_000));
    }

    #[test]
    fn calculate_tx_l1_cost_mantle_type2() {
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
        let l1_block_info = L1BlockInfo {
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
        // TIPS: the Bedrock's l1GasUsed added the overhead, so we need to subtract it
        // <https://github.com/ethereum-optimism/op-geth/blob/v1.101411.0/core/types/rollup_cost.go#L206>
        let gas_used = l1_block_info.data_gas(TX, MantleSpecId::CANCUN)
            + l1_block_info.l1_fee_overhead.unwrap_or_default();
        assert_eq!(gas_used, expected_l1_gas_used);

        let l1_fee = l1_block_info.calculate_tx_l1_cost(TX, MantleSpecId::CANCUN);
        assert_eq!(l1_fee, expected_l1_fee)
    }

    #[test]
    fn calculate_tx_l1_cost_mantle_type0() {
        // rig
        //
        // <https://mantlescan.xyz/block/70718078>
        //
        // decoded from
        let l1_block_info = L1BlockInfo {
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
        let gas_used = l1_block_info.data_gas(TX, MantleSpecId::CANCUN)
            + l1_block_info.l1_fee_overhead.unwrap_or_default();
        assert_eq!(gas_used, expected_l1_gas_used);

        let l1_fee = l1_block_info.calculate_tx_l1_cost(TX, MantleSpecId::CANCUN);
        assert_eq!(l1_fee, expected_l1_fee)
    }

    #[test]
    fn calculate_tx_l1_cost_ecotone() {
        // rig

        // l1 block info for OP mainnet ecotone block 118024092
        // 1710374401 (ecotone timestamp)
        // 1711603765 (block 118024092 timestamp)
        // 1720627201 (fjord timestamp)
        // <https://optimistic.etherscan.io/block/118024092>
        // decoded from
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from_be_bytes(hex!(
                "0000000000000000000000000000000000000000000000000000000af39ac327"
            )), // 47036678951
            l1_base_fee_scalar: U256::from(1368),
            l1_blob_base_fee: Some(U256::from_be_bytes(hex!(
                "0000000000000000000000000000000000000000000000000000000d5ea528d2"
            ))), // 57422457042
            l1_blob_base_fee_scalar: Some(U256::from(810949)),
            ..Default::default()
        };

        // second tx in OP mainnet ecotone block 118024092
        // <https://optimistic.etherscan.io/tx/0xa75ef696bf67439b4d5b61da85de9f3ceaa2e145abe982212101b244b63749c2>
        const TX: &[u8] = &hex!("02f8b30a832253fc8402d11f39842c8a46398301388094dc6ff44d5d932cbd77b52e5612ba0529dc6226f180b844a9059cbb000000000000000000000000d43e02db81f4d46cdf8521f623d21ea0ec7562a50000000000000000000000000000000000000000000000008ac7230489e80000c001a02947e24750723b48f886931562c55d9e07f856d8e06468e719755e18bbc3a570a0784da9ce59fd7754ea5be6e17a86b348e441348cd48ace59d174772465eadbd1");

        // l1 gas used for tx and l1 fee for tx, from OP mainnet block scanner
        // <https://optimistic.etherscan.io/tx/0xa75ef696bf67439b4d5b61da85de9f3ceaa2e145abe982212101b244b63749c2>
        let expected_l1_gas_used = U256::from(2456);
        let expected_l1_fee = U256::from_be_bytes(hex!(
            "000000000000000000000000000000000000000000000000000006a510bd7431" // 7306020222001 wei
        ));

        // test

        let gas_used = l1_block_info.data_gas(TX, MantleSpecId::ECOTONE);

        assert_eq!(gas_used, expected_l1_gas_used);

        let l1_fee = l1_block_info.calculate_tx_l1_cost_ecotone(TX, MantleSpecId::ECOTONE);

        assert_eq!(l1_fee, expected_l1_fee)
    }

    #[test]
    fn test_calculate_tx_l1_cost_fjord() {
        // l1FeeScaled = baseFeeScalar*l1BaseFee*16 + blobFeeScalar*l1BlobBaseFee
        //             = 1000 * 1000 * 16 + 1000 * 1000
        //             = 17e6
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1_000),
            l1_base_fee_scalar: U256::from(1_000),
            l1_blob_base_fee: Some(U256::from(1_000)),
            l1_blob_base_fee_scalar: Some(U256::from(1_000)),
            ..Default::default()
        };

        // fastLzSize = 4
        // estimatedSize = max(minTransactionSize, intercept + fastlzCoef*fastlzSize)
        //               = max(100e6, 836500*4 - 42585600)
        //               = 100e6
        let input = bytes!("FACADE");
        // l1Cost = estimatedSize * l1FeeScaled / 1e12
        //        = 100e6 * 17 / 1e6
        //        = 1700
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::FJORD);
        assert_eq!(gas_cost, U256::from(1700));

        // fastLzSize = 202
        // estimatedSize = max(minTransactionSize, intercept + fastlzCoef*fastlzSize)
        //               = max(100e6, 836500*202 - 42585600)
        //               = 126387400
        let input = bytes!("02f901550a758302df1483be21b88304743f94f80e51afb613d764fa61751affd3313c190a86bb870151bd62fd12adb8e41ef24f3f000000000000000000000000000000000000000000000000000000000000006e000000000000000000000000af88d065e77c8cc2239327c5edb3a432268e5831000000000000000000000000000000000000000000000000000000000003c1e5000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000148c89ed219d02f1a5be012c689b4f5b731827bebe000000000000000000000000c001a033fd89cb37c31b2cba46b6466e040c61fc9b2a3675a7f5f493ebd5ad77c497f8a07cdf65680e238392693019b4092f610222e71b7cec06449cb922b93b6a12744e");
        // l1Cost = estimatedSize * l1FeeScaled / 1e12
        //        = 126387400 * 17 / 1e6
        //        = 2148
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::FJORD);
        assert_eq!(gas_cost, U256::from(2148));

        // Zero rollup data gas cost should result in zero
        let input = bytes!("");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::FJORD);
        assert_eq!(gas_cost, U256::ZERO);

        // Deposit transactions with the EIP-2718 type of 0x7F should result in zero
        let input = bytes!("7FFACADE");
        let gas_cost = l1_block_info.calculate_tx_l1_cost(&input, MantleSpecId::FJORD);
        assert_eq!(gas_cost, U256::ZERO);
    }

    #[test]
    fn calculate_tx_l1_cost_fjord() {
        // rig

        // L1 block info for OP mainnet fjord block 124665056
        // <https://optimistic.etherscan.io/block/124665056>
        let l1_block_info = L1BlockInfo {
            l1_base_fee: U256::from(1055991687),
            l1_base_fee_scalar: U256::from(5227),
            l1_blob_base_fee_scalar: Some(U256::from(1014213)),
            l1_blob_base_fee: Some(U256::from(1)),
            ..Default::default() // l1 fee overhead (l1 gas used) deprecated since Fjord
        };

        // second tx in OP mainnet Fjord block 124665056
        // <https://optimistic.etherscan.io/tx/0x1059e8004daff32caa1f1b1ef97fe3a07a8cf40508f5b835b66d9420d87c4a4a>
        const TX: &[u8] = &hex!("02f904940a8303fba78401d6d2798401db2b6d830493e0943e6f4f7866654c18f536170780344aa8772950b680b904246a761202000000000000000000000000087000a300de7200382b55d40045000000e5d60e0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003a0000000000000000000000000000000000000000000000000000000000000022482ad56cb0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000120000000000000000000000000dc6ff44d5d932cbd77b52e5612ba0529dc6226f1000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000044095ea7b300000000000000000000000021c4928109acb0659a88ae5329b5374a3024694c0000000000000000000000000000000000000000000000049b9ca9a6943400000000000000000000000000000000000000000000000000000000000000000000000000000000000021c4928109acb0659a88ae5329b5374a3024694c000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000000000024b6b55f250000000000000000000000000000000000000000000000049b9ca9a694340000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000415ec214a3950bea839a7e6fbb0ba1540ac2076acd50820e2d5ef83d0902cdffb24a47aff7de5190290769c4f0a9c6fabf63012986a0d590b1b571547a8c7050ea1b00000000000000000000000000000000000000000000000000000000000000c080a06db770e6e25a617fe9652f0958bd9bd6e49281a53036906386ed39ec48eadf63a07f47cf51a4a40b4494cf26efc686709a9b03939e20ee27e59682f5faa536667e");

        // l1 gas used for tx and l1 fee for tx, from OP mainnet block scanner
        // https://optimistic.etherscan.io/tx/0x1059e8004daff32caa1f1b1ef97fe3a07a8cf40508f5b835b66d9420d87c4a4a
        let expected_data_gas = U256::from(4471);
        let expected_l1_fee = U256::from_be_bytes(hex!(
            "00000000000000000000000000000000000000000000000000000005bf1ab43d"
        ));

        // test

        let data_gas = l1_block_info.data_gas(TX, MantleSpecId::FJORD);

        assert_eq!(data_gas, expected_data_gas);

        let l1_fee = l1_block_info.calculate_tx_l1_cost_fjord(TX);

        assert_eq!(l1_fee, expected_l1_fee)
    }
}
