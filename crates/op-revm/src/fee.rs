

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeeModel {
    //the gas used by the rollup in the L2 transaction. 
    pub rollup_cost: u64,
    //the gas used by operator fee in the L2 transaction.
    pub operator_cost: u64,
}


impl FeeModel {
    pub fn total_cost(&self) -> u64 {
        self.rollup_cost + self.operator_cost
    }
}