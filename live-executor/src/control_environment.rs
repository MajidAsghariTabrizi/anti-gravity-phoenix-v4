use std::error::Error;
use std::fmt::{Display, Formatter};

pub const WALLET_ADDRESS_ENV: &str = "WALLET_ADDRESS";
pub const EXECUTOR_ADDRESS_ENV: &str = "EXECUTOR_ADDRESS";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlAddressEnvironment {
    pub wallet_address: String,
    pub executor_address: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingEnvironment {
    name: &'static str,
}

impl MissingEnvironment {
    pub const fn name(self) -> &'static str {
        self.name
    }
}

impl Display for MissingEnvironment {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "required environment is missing: {}", self.name)
    }
}

impl Error for MissingEnvironment {}

pub fn required_environment_with(
    name: &'static str,
    lookup: &mut impl FnMut(&'static str) -> Option<String>,
) -> Result<String, MissingEnvironment> {
    lookup(name)
        .filter(|value| !value.is_empty())
        .ok_or(MissingEnvironment { name })
}

pub fn control_address_environment_with(
    mut lookup: impl FnMut(&'static str) -> Option<String>,
) -> Result<ControlAddressEnvironment, MissingEnvironment> {
    Ok(ControlAddressEnvironment {
        wallet_address: required_environment_with(WALLET_ADDRESS_ENV, &mut lookup)?,
        executor_address: required_environment_with(EXECUTOR_ADDRESS_ENV, &mut lookup)?,
    })
}
