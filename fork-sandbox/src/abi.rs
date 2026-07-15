use ethabi::Contract;
use std::io::Cursor;

pub(crate) fn executor_contract() -> Result<Contract, ethabi::Error> {
    Contract::load(Cursor::new(include_bytes!("../abi/PhoenixExecutor.json")))
}
