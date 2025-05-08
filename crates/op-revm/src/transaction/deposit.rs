use revm::primitives::B256;

pub const DEPOSIT_TRANSACTION_TYPE: u8 = 0x7E;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DepositTransactionParts {
    pub source_hash: B256,
    pub mint: Option<u128>,
    pub is_system_transaction: bool,
    // EthValue means L2 BVM_ETH mint tag, nil means that there is no need to mint BVM_ETH.
    pub eth_value: Option<u128>,
    // EthTxValue means L2 BVM_ETH tx tag, nil means that there is no need to transfer BVM_ETH to msg.To.
    pub eth_tx_value: Option<u128>,
}

impl DepositTransactionParts {
    pub fn new(source_hash: B256, mint: Option<u128>, is_system_transaction: bool, eth_value: Option<u128>, eth_tx_value: Option<u128>) -> Self {
        Self {
            source_hash,
            mint,
            is_system_transaction,
            eth_value,
            eth_tx_value,
        }
    }
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use revm::primitives::b256;

    #[test]
    fn serialize_json_deposit_tx_parts() {
        let response = r#"{"source_hash":"0xe927a1448525fb5d32cb50ee1408461a945ba6c39bd5cf5621407d500ecc8de9","mint":52,"is_system_transaction":false}"#;

        let deposit_tx_parts: DepositTransactionParts = serde_json::from_str(response).unwrap();
        assert_eq!(
            deposit_tx_parts,
            DepositTransactionParts::new(
                b256!("0xe927a1448525fb5d32cb50ee1408461a945ba6c39bd5cf5621407d500ecc8de9"),
                Some(0x34),
                false,
                None,
                None,
            )
        );
    }

    #[test]
    fn serialize_json_deposit_tx_parts_with_bvm_eth() {
        let response = r#"{"source_hash":"0xe927a1448525fb5d32cb50ee1408461a945ba6c39bd5cf5621407d500ecc8de9","mint":52,"is_system_transaction":false,"eth_value":100,"eth_tx_value":100}"#;

        let deposit_tx_parts: DepositTransactionParts = serde_json::from_str(response).unwrap();
        assert_eq!(deposit_tx_parts,
            DepositTransactionParts::new(
                b256!("0xe927a1448525fb5d32cb50ee1408461a945ba6c39bd5cf5621407d500ecc8de9"),
                Some(0x34),
                false,
                Some(100),
                Some(100),
            )
        );
    }
}
