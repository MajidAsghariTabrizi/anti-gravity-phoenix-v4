#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EthCall {
    pub target: String,
    pub calldata: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticallBatch {
    pub calls: Vec<EthCall>,
}

impl MulticallBatch {
    pub fn new(calls: Vec<EthCall>) -> Self {
        Self { calls }
    }

    pub fn is_semantically_safe(&self) -> bool {
        self.calls
            .iter()
            .all(|call| call.target.starts_with("0x") && call.calldata.starts_with("0x"))
    }
}

