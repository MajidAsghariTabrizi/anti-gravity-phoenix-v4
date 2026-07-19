use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Address(String);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Amount(pub u128);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PoolId(pub String);

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("address is not canonical")]
pub struct AddressParseError;

impl Address {
    pub fn parse(input: &str) -> Result<Self, AddressParseError> {
        let canonical = input.to_ascii_lowercase();
        if canonical.len() != 42
            || !canonical.starts_with("0x")
            || !canonical[2..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AddressParseError);
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
