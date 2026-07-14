use super::{
    DecodedSwap, DecodedSwapKind, OriginEvidence, OuterSelectorKind, RouterKind, UnsupportedReason,
    WrapperKind,
};
use crate::domain::{Address, Amount, PoolId};
use ethabi::{ParamType, Token};
use std::sync::OnceLock;

const MAX_CALLDATA_BYTES: usize = 256 * 1024;
const MAX_INNER_CALLS: usize = 16;
const MAX_TOTAL_NESTED_BYTES: usize = 128 * 1024;
const MAX_V3_HOPS: usize = 8;
const MAX_V3_PATH_BYTES: usize = 20 + 23 * MAX_V3_HOPS;
const MAX_PERMIT_DETAILS: usize = 16;
const MAX_SIGNATURE_BYTES: usize = 4 * 1024;
const UNIVERSAL_COMMAND_MASK: u8 = 0x3f;
const UNIVERSAL_ALLOW_REVERT: u8 = 0x80;

pub(super) enum DecodeOutcome {
    Supported(DecodedSwap),
    Unsupported(OriginEvidence),
    Malformed(OriginEvidence),
}

#[derive(Clone, Debug)]
struct Selectors {
    legacy_exact_input_single: [u8; 4],
    legacy_exact_input: [u8; 4],
    legacy_exact_output_single: [u8; 4],
    legacy_exact_output: [u8; 4],
    legacy_multicall: [u8; 4],
    router02_exact_input_single: [u8; 4],
    universal_execute: [u8; 4],
    universal_execute_deadline: [u8; 4],
    refund_eth: [u8; 4],
    unwrap_weth9: [u8; 4],
    sweep_token: [u8; 4],
    unwrap_weth9_with_fee: [u8; 4],
    sweep_token_with_fee: [u8; 4],
    self_permit: [u8; 4],
    self_permit_if_necessary: [u8; 4],
    self_permit_allowed: [u8; 4],
    self_permit_allowed_if_necessary: [u8; 4],
}

static SELECTORS: OnceLock<Selectors> = OnceLock::new();

pub(super) fn classify(router_kind: RouterKind, calldata: &str) -> DecodeOutcome {
    let bytes = match decode_calldata(calldata) {
        Ok(bytes) => bytes,
        Err(()) => {
            return DecodeOutcome::Malformed(malformed_evidence(
                router_kind,
                OuterSelectorKind::Unknown,
                WrapperKind::None,
            ));
        }
    };
    let selector: [u8; 4] = bytes[0..4].try_into().expect("selector length is fixed");
    let data = &bytes[4..];
    match router_kind {
        RouterKind::LegacySwapRouter => classify_legacy(selector, data),
        RouterKind::SwapRouter02 => classify_router02(selector, data),
        RouterKind::UniversalRouter => classify_universal(selector, data),
    }
}

fn classify_legacy(selector: [u8; 4], data: &[u8]) -> DecodeOutcome {
    let known = selectors();
    if selector == known.legacy_exact_input_single {
        let evidence = OriginEvidence::new(
            RouterKind::LegacySwapRouter,
            OuterSelectorKind::LegacyExactInputSingle,
            WrapperKind::Direct,
        );
        return finish_supported(
            decode_legacy_exact_input_single(data),
            evidence,
            vec!["exactInputSingle".to_string()],
        );
    }
    if selector == known.legacy_exact_input {
        let evidence = OriginEvidence::new(
            RouterKind::LegacySwapRouter,
            OuterSelectorKind::LegacyExactInput,
            WrapperKind::Direct,
        );
        return finish_supported(
            decode_legacy_exact_input(data),
            evidence,
            vec!["exactInput".to_string()],
        );
    }
    if selector == known.legacy_multicall {
        return decode_legacy_multicall(data);
    }
    if selector == known.legacy_exact_output_single || selector == known.legacy_exact_output {
        return DecodeOutcome::Unsupported(unsupported_evidence(
            RouterKind::LegacySwapRouter,
            if selector == known.legacy_exact_output_single {
                OuterSelectorKind::LegacyExactOutputSingle
            } else {
                OuterSelectorKind::Unknown
            },
            WrapperKind::Direct,
            UnsupportedReason::ExactOutput,
            1,
        ));
    }
    DecodeOutcome::Unsupported(unsupported_evidence(
        RouterKind::LegacySwapRouter,
        OuterSelectorKind::Unknown,
        WrapperKind::Direct,
        UnsupportedReason::UnknownSelector,
        1,
    ))
}

fn classify_router02(selector: [u8; 4], data: &[u8]) -> DecodeOutcome {
    if selector != selectors().router02_exact_input_single {
        return DecodeOutcome::Unsupported(unsupported_evidence(
            RouterKind::SwapRouter02,
            OuterSelectorKind::Unknown,
            WrapperKind::Direct,
            UnsupportedReason::UnknownSelector,
            1,
        ));
    }
    let evidence = OriginEvidence::new(
        RouterKind::SwapRouter02,
        OuterSelectorKind::SwapRouter02ExactInputSingle,
        WrapperKind::Direct,
    );
    finish_supported(
        decode_router02_exact_input_single(data),
        evidence,
        vec!["exactInputSingle".to_string()],
    )
}

fn classify_universal(selector: [u8; 4], data: &[u8]) -> DecodeOutcome {
    let (selector_kind, parameter_types) = if selector == selectors().universal_execute {
        (
            OuterSelectorKind::UniversalExecute,
            vec![
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
            ],
        )
    } else if selector == selectors().universal_execute_deadline {
        (
            OuterSelectorKind::UniversalExecuteWithDeadline,
            vec![
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
                ParamType::Uint(256),
            ],
        )
    } else {
        return DecodeOutcome::Unsupported(unsupported_evidence(
            RouterKind::UniversalRouter,
            OuterSelectorKind::Unknown,
            WrapperKind::UniversalRouter,
            UnsupportedReason::UnknownSelector,
            1,
        ));
    };

    let mut evidence = OriginEvidence::new(
        RouterKind::UniversalRouter,
        selector_kind,
        WrapperKind::UniversalRouter,
    );
    let tokens = match decode_canonical(&parameter_types, data) {
        Ok(tokens) => tokens,
        Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
    };
    let commands = match token_bytes(tokens.first()) {
        Ok(commands) => commands,
        Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
    };
    let inputs = match token_bytes_array(tokens.get(1)) {
        Ok(inputs) => inputs,
        Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
    };
    evidence.command_count = commands.len();
    if commands.is_empty()
        || commands.len() > MAX_INNER_CALLS
        || commands.len() != inputs.len()
        || !nested_bytes_are_bounded(&inputs)
    {
        return DecodeOutcome::Malformed(mark_malformed(evidence));
    }

    let command_types = commands
        .iter()
        .map(|command| command & UNIVERSAL_COMMAND_MASK)
        .collect::<Vec<_>>();
    let swap_count = command_types
        .iter()
        .filter(|command| matches!(**command, 0x00 | 0x01 | 0x08 | 0x09 | 0x10))
        .count();
    if swap_count > 1 {
        return DecodeOutcome::Unsupported(mark_unsupported(
            evidence,
            UnsupportedReason::AmbiguousMultiSwap,
        ));
    }

    let mut decoded_commands = vec![match selector_kind {
        OuterSelectorKind::UniversalExecute => "execute".to_string(),
        OuterSelectorKind::UniversalExecuteWithDeadline => "executeWithDeadline".to_string(),
        _ => unreachable!("universal selector kind is fixed"),
    }];
    let mut supported_swap = None;
    for ((command_byte, command_type), input) in
        commands.iter().zip(command_types.iter()).zip(inputs.iter())
    {
        match *command_type {
            0x00 => {
                if command_byte & UNIVERSAL_ALLOW_REVERT != 0 {
                    return DecodeOutcome::Unsupported(mark_unsupported(
                        evidence,
                        UnsupportedReason::OptionalSwap,
                    ));
                }
                let swap = match decode_universal_v3_exact_in(input) {
                    Ok(swap) => swap,
                    Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
                };
                decoded_commands.push("V3_SWAP_EXACT_IN".to_string());
                supported_swap = Some(swap);
            }
            0x01 => {
                return DecodeOutcome::Unsupported(mark_unsupported(
                    evidence,
                    UnsupportedReason::ExactOutput,
                ));
            }
            0x08 | 0x09 | 0x10 => {
                return DecodeOutcome::Unsupported(mark_unsupported(
                    evidence,
                    UnsupportedReason::UnsupportedSwapFamily,
                ));
            }
            0x21 => {
                return DecodeOutcome::Unsupported(mark_unsupported(
                    evidence,
                    UnsupportedReason::NestedSubPlan,
                ));
            }
            command if is_universal_companion(command) => {
                if validate_universal_companion(command, input).is_err() {
                    return DecodeOutcome::Malformed(mark_malformed(evidence));
                }
                decoded_commands.push(universal_companion_name(command).to_string());
            }
            _ => {
                return DecodeOutcome::Unsupported(mark_unsupported(
                    evidence,
                    UnsupportedReason::UnknownCommand,
                ));
            }
        }
    }

    let Some(swap) = supported_swap else {
        return DecodeOutcome::Unsupported(mark_unsupported(
            evidence,
            UnsupportedReason::MissingSwap,
        ));
    };
    DecodeOutcome::Supported(swap.into_decoded(evidence, decoded_commands))
}

fn decode_legacy_multicall(data: &[u8]) -> DecodeOutcome {
    let mut evidence = OriginEvidence::new(
        RouterKind::LegacySwapRouter,
        OuterSelectorKind::LegacyMulticall,
        WrapperKind::Multicall,
    );
    let tokens = match decode_canonical(&[ParamType::Array(Box::new(ParamType::Bytes))], data) {
        Ok(tokens) => tokens,
        Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
    };
    let calls = match token_bytes_array(tokens.first()) {
        Ok(calls) => calls,
        Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
    };
    evidence.command_count = calls.len();
    if calls.is_empty() || calls.len() > MAX_INNER_CALLS || !nested_bytes_are_bounded(&calls) {
        return DecodeOutcome::Malformed(mark_malformed(evidence));
    }

    let mut parsed_calls = Vec::with_capacity(calls.len());
    let mut swap_count = 0_usize;
    for call in &calls {
        if call.len() < 4 {
            return DecodeOutcome::Malformed(mark_malformed(evidence));
        }
        let selector: [u8; 4] = call[0..4].try_into().expect("selector length is fixed");
        if is_legacy_swap_selector(selector) {
            swap_count += 1;
        }
        parsed_calls.push((selector, &call[4..]));
    }
    if swap_count > 1 {
        return DecodeOutcome::Unsupported(mark_unsupported(
            evidence,
            UnsupportedReason::AmbiguousMultiSwap,
        ));
    }

    let known = selectors();
    let mut supported_swap = None;
    let mut decoded_commands = vec!["multicall".to_string()];
    for (selector, call_data) in parsed_calls {
        if selector == known.legacy_exact_input_single {
            let swap = match decode_legacy_exact_input_single(call_data) {
                Ok(swap) => swap,
                Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
            };
            decoded_commands.push("exactInputSingle".to_string());
            supported_swap = Some(swap);
        } else if selector == known.legacy_exact_input {
            let swap = match decode_legacy_exact_input(call_data) {
                Ok(swap) => swap,
                Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
            };
            decoded_commands.push("exactInput".to_string());
            supported_swap = Some(swap);
        } else if selector == known.legacy_exact_output_single
            || selector == known.legacy_exact_output
        {
            return DecodeOutcome::Unsupported(mark_unsupported(
                evidence,
                UnsupportedReason::ExactOutput,
            ));
        } else if selector == known.legacy_multicall {
            return DecodeOutcome::Unsupported(mark_unsupported(
                evidence,
                UnsupportedReason::NestedSubPlan,
            ));
        } else {
            match validate_legacy_companion(selector, call_data) {
                Ok(Some(name)) => decoded_commands.push(name.to_string()),
                Ok(None) => {
                    return DecodeOutcome::Unsupported(mark_unsupported(
                        evidence,
                        UnsupportedReason::UnknownCommand,
                    ));
                }
                Err(()) => return DecodeOutcome::Malformed(mark_malformed(evidence)),
            }
        }
    }
    let Some(swap) = supported_swap else {
        return DecodeOutcome::Unsupported(mark_unsupported(
            evidence,
            UnsupportedReason::MissingSwap,
        ));
    };
    DecodeOutcome::Supported(swap.into_decoded(evidence, decoded_commands))
}

#[derive(Clone, Debug)]
struct SwapData {
    path: V3Path,
    amount_in: Amount,
    kind: DecodedSwapKind,
}

impl SwapData {
    fn into_decoded(
        self,
        mut evidence: OriginEvidence,
        decoded_commands: Vec<String>,
    ) -> DecodedSwap {
        evidence.decoded_swap_kind = self.kind;
        evidence.v3_hop_count = self.path.touched_pools.len();
        evidence.exact_in = Some(true);
        evidence.supported = true;
        evidence.unsupported_reason = UnsupportedReason::None;
        DecodedSwap {
            decoded_commands,
            swap_path: self.path.tokens,
            amount_in: self.amount_in,
            touched_pools: self.path.touched_pools,
            evidence,
        }
    }
}

#[derive(Clone, Debug)]
struct V3Path {
    tokens: Vec<Address>,
    touched_pools: Vec<PoolId>,
}

fn decode_legacy_exact_input_single(data: &[u8]) -> Result<SwapData, ()> {
    let tokens = decode_canonical(&[legacy_exact_input_single_type()], data)?;
    let values = token_tuple(tokens.first())?;
    if values.len() != 8 {
        return Err(());
    }
    let token_in = required_address(values.first())?;
    let token_out = required_address(values.get(1))?;
    let fee = bounded_uint(values.get(2), 24)?.low_u32();
    let _recipient = required_address(values.get(3))?;
    let _deadline = bounded_uint(values.get(4), 256)?;
    let amount_in = concrete_amount(values.get(5))?;
    let _amount_out_minimum = bounded_uint(values.get(6), 256)?;
    let _sqrt_price_limit = bounded_uint(values.get(7), 160)?;
    single_hop_swap(
        token_in,
        token_out,
        fee,
        amount_in,
        DecodedSwapKind::V3ExactInputSingle,
    )
}

fn decode_router02_exact_input_single(data: &[u8]) -> Result<SwapData, ()> {
    let tokens = decode_canonical(&[router02_exact_input_single_type()], data)?;
    let values = token_tuple(tokens.first())?;
    if values.len() != 7 {
        return Err(());
    }
    let token_in = required_address(values.first())?;
    let token_out = required_address(values.get(1))?;
    let fee = bounded_uint(values.get(2), 24)?.low_u32();
    let _recipient = required_address(values.get(3))?;
    let amount_in = concrete_amount(values.get(4))?;
    let _amount_out_minimum = bounded_uint(values.get(5), 256)?;
    let _sqrt_price_limit = bounded_uint(values.get(6), 160)?;
    single_hop_swap(
        token_in,
        token_out,
        fee,
        amount_in,
        DecodedSwapKind::V3ExactInputSingle,
    )
}

fn decode_legacy_exact_input(data: &[u8]) -> Result<SwapData, ()> {
    let tokens = decode_canonical(
        &[ParamType::Tuple(vec![
            ParamType::Bytes,
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(256),
        ])],
        data,
    )?;
    let values = token_tuple(tokens.first())?;
    if values.len() != 5 {
        return Err(());
    }
    let path = decode_v3_path(token_bytes(values.first())?)?;
    let _recipient = required_address(values.get(1))?;
    let _deadline = bounded_uint(values.get(2), 256)?;
    let amount_in = concrete_amount(values.get(3))?;
    let _amount_out_minimum = bounded_uint(values.get(4), 256)?;
    Ok(SwapData {
        path,
        amount_in,
        kind: DecodedSwapKind::V3ExactInput,
    })
}

fn decode_universal_v3_exact_in(data: &[u8]) -> Result<SwapData, ()> {
    let tokens = decode_canonical(
        &[
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Bytes,
            ParamType::Bool,
        ],
        data,
    )?;
    if tokens.len() != 5 {
        return Err(());
    }
    let _recipient = token_address(tokens.first())?;
    let amount_in = concrete_amount(tokens.get(1))?;
    let _amount_out_minimum = bounded_uint(tokens.get(2), 256)?;
    let path = decode_v3_path(token_bytes(tokens.get(3))?)?;
    let _payer_is_user = token_bool(tokens.get(4))?;
    Ok(SwapData {
        path,
        amount_in,
        kind: DecodedSwapKind::V3ExactInput,
    })
}

fn single_hop_swap(
    token_in: Address,
    token_out: Address,
    fee: u32,
    amount_in: Amount,
    kind: DecodedSwapKind,
) -> Result<SwapData, ()> {
    if token_in == token_out || fee == 0 {
        return Err(());
    }
    let touched_pool = canonical_pool_id(&token_in, &token_out, fee);
    Ok(SwapData {
        path: V3Path {
            tokens: vec![token_in, token_out],
            touched_pools: vec![touched_pool],
        },
        amount_in,
        kind,
    })
}

fn decode_v3_path(path: &[u8]) -> Result<V3Path, ()> {
    if path.len() < 43 || path.len() > MAX_V3_PATH_BYTES || (path.len() - 20) % 23 != 0 {
        return Err(());
    }
    let hop_count = (path.len() - 20) / 23;
    if hop_count == 0 || hop_count > MAX_V3_HOPS {
        return Err(());
    }
    let mut tokens = Vec::with_capacity(hop_count + 1);
    let mut touched_pools = Vec::with_capacity(hop_count);
    let mut current = address_from_bytes(&path[0..20])?;
    tokens.push(current.clone());
    for hop in 0..hop_count {
        let offset = 20 + hop * 23;
        let fee = u32::from_be_bytes([0, path[offset], path[offset + 1], path[offset + 2]]);
        let next = address_from_bytes(&path[offset + 3..offset + 23])?;
        if fee == 0 || current == next {
            return Err(());
        }
        touched_pools.push(canonical_pool_id(&current, &next, fee));
        tokens.push(next.clone());
        current = next;
    }
    Ok(V3Path {
        tokens,
        touched_pools,
    })
}

fn canonical_pool_id(token_a: &Address, token_b: &Address, fee: u32) -> PoolId {
    let (token0, token1) = if token_a.as_str() < token_b.as_str() {
        (token_a, token_b)
    } else {
        (token_b, token_a)
    };
    PoolId(format!("{}:{}:{fee}", token0.as_str(), token1.as_str()))
}

fn validate_legacy_companion(selector: [u8; 4], data: &[u8]) -> Result<Option<&'static str>, ()> {
    let known = selectors();
    let (name, types) = if selector == known.refund_eth {
        ("refundETH", vec![])
    } else if selector == known.unwrap_weth9 {
        (
            "unwrapWETH9",
            vec![ParamType::Uint(256), ParamType::Address],
        )
    } else if selector == known.sweep_token {
        (
            "sweepToken",
            vec![ParamType::Address, ParamType::Uint(256), ParamType::Address],
        )
    } else if selector == known.unwrap_weth9_with_fee {
        (
            "unwrapWETH9WithFee",
            vec![
                ParamType::Uint(256),
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
            ],
        )
    } else if selector == known.sweep_token_with_fee {
        (
            "sweepTokenWithFee",
            vec![
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
            ],
        )
    } else if selector == known.self_permit
        || selector == known.self_permit_if_necessary
        || selector == known.self_permit_allowed
        || selector == known.self_permit_allowed_if_necessary
    {
        (
            if selector == known.self_permit {
                "selfPermit"
            } else if selector == known.self_permit_if_necessary {
                "selfPermitIfNecessary"
            } else if selector == known.self_permit_allowed {
                "selfPermitAllowed"
            } else {
                "selfPermitAllowedIfNecessary"
            },
            vec![
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Uint(256),
                ParamType::Uint(8),
                ParamType::FixedBytes(32),
                ParamType::FixedBytes(32),
            ],
        )
    } else {
        return Ok(None);
    };
    decode_canonical(&types, data)?;
    Ok(Some(name))
}

fn is_universal_companion(command: u8) -> bool {
    matches!(command, 0x02..=0x06 | 0x0a..=0x0e)
}

fn universal_companion_name(command: u8) -> &'static str {
    match command {
        0x02 => "PERMIT2_TRANSFER_FROM",
        0x03 => "PERMIT2_PERMIT_BATCH",
        0x04 => "SWEEP",
        0x05 => "TRANSFER",
        0x06 => "PAY_PORTION",
        0x0a => "PERMIT2_PERMIT",
        0x0b => "WRAP_ETH",
        0x0c => "UNWRAP_WETH",
        0x0d => "PERMIT2_TRANSFER_FROM_BATCH",
        0x0e => "BALANCE_CHECK_ERC20",
        _ => unreachable!("companion command is prevalidated"),
    }
}

fn validate_universal_companion(command: u8, data: &[u8]) -> Result<(), ()> {
    match command {
        0x02 => decode_canonical(
            &[ParamType::Address, ParamType::Address, ParamType::Uint(160)],
            data,
        )
        .map(|_| ()),
        0x03 => {
            let tokens = decode_canonical(&[permit_batch_type(), ParamType::Bytes], data)?;
            bounded_permit_batch(tokens.first())?;
            bounded_signature(tokens.get(1))
        }
        0x04..=0x06 | 0x0e => decode_canonical(
            &[ParamType::Address, ParamType::Address, ParamType::Uint(256)],
            data,
        )
        .map(|_| ()),
        0x0a => {
            let tokens = decode_canonical(&[permit_single_type(), ParamType::Bytes], data)?;
            bounded_signature(tokens.get(1))
        }
        0x0b | 0x0c => {
            decode_canonical(&[ParamType::Address, ParamType::Uint(256)], data).map(|_| ())
        }
        0x0d => {
            let tokens = decode_canonical(
                &[ParamType::Array(Box::new(allowance_transfer_detail_type()))],
                data,
            )?;
            match tokens.first() {
                Some(Token::Array(values))
                    if !values.is_empty() && values.len() <= MAX_PERMIT_DETAILS =>
                {
                    Ok(())
                }
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

fn bounded_permit_batch(token: Option<&Token>) -> Result<(), ()> {
    let values = token_tuple(token)?;
    let details = match values.first() {
        Some(Token::Array(details)) => details,
        _ => return Err(()),
    };
    if details.is_empty() || details.len() > MAX_PERMIT_DETAILS {
        return Err(());
    }
    Ok(())
}

fn bounded_signature(token: Option<&Token>) -> Result<(), ()> {
    match token {
        Some(Token::Bytes(value)) if !value.is_empty() && value.len() <= MAX_SIGNATURE_BYTES => {
            Ok(())
        }
        _ => Err(()),
    }
}

fn finish_supported(
    decoded: Result<SwapData, ()>,
    evidence: OriginEvidence,
    commands: Vec<String>,
) -> DecodeOutcome {
    match decoded {
        Ok(decoded) => DecodeOutcome::Supported(decoded.into_decoded(evidence, commands)),
        Err(()) => DecodeOutcome::Malformed(mark_malformed(evidence)),
    }
}

fn is_legacy_swap_selector(selector: [u8; 4]) -> bool {
    let known = selectors();
    selector == known.legacy_exact_input_single
        || selector == known.legacy_exact_input
        || selector == known.legacy_exact_output_single
        || selector == known.legacy_exact_output
}

fn decode_calldata(calldata: &str) -> Result<Vec<u8>, ()> {
    let encoded = calldata.strip_prefix("0x").ok_or(())?;
    if encoded.len() < 8
        || encoded.len() > MAX_CALLDATA_BYTES * 2
        || encoded.len() % 2 != 0
        || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(());
    }
    hex::decode(encoded).map_err(|_| ())
}

fn decode_canonical(types: &[ParamType], data: &[u8]) -> Result<Vec<Token>, ()> {
    let tokens = ethabi::decode(types, data).map_err(|_| ())?;
    if ethabi::encode(&tokens) != data {
        return Err(());
    }
    Ok(tokens)
}

fn nested_bytes_are_bounded(values: &[Vec<u8>]) -> bool {
    values
        .iter()
        .try_fold(0_usize, |total, value| {
            if value.len() > MAX_CALLDATA_BYTES {
                return None;
            }
            total.checked_add(value.len())
        })
        .is_some_and(|total| total <= MAX_TOTAL_NESTED_BYTES)
}

fn token_tuple(token: Option<&Token>) -> Result<&[Token], ()> {
    match token {
        Some(Token::Tuple(values)) => Ok(values),
        _ => Err(()),
    }
}

fn token_bytes(token: Option<&Token>) -> Result<&[u8], ()> {
    match token {
        Some(Token::Bytes(value)) => Ok(value),
        _ => Err(()),
    }
}

fn token_bytes_array(token: Option<&Token>) -> Result<Vec<Vec<u8>>, ()> {
    let values = match token {
        Some(Token::Array(values)) => values,
        _ => return Err(()),
    };
    values
        .iter()
        .map(|value| match value {
            Token::Bytes(bytes) => Ok(bytes.clone()),
            _ => Err(()),
        })
        .collect()
}

fn token_address(token: Option<&Token>) -> Result<Address, ()> {
    match token {
        Some(Token::Address(value)) => {
            Address::parse(&format!("0x{}", hex::encode(value.as_bytes()))).map_err(|_| ())
        }
        _ => Err(()),
    }
}

fn required_address(token: Option<&Token>) -> Result<Address, ()> {
    let address = token_address(token)?;
    if address.as_str() == "0x0000000000000000000000000000000000000000" {
        return Err(());
    }
    Ok(address)
}

fn address_from_bytes(bytes: &[u8]) -> Result<Address, ()> {
    if bytes.len() != 20 || bytes.iter().all(|byte| *byte == 0) {
        return Err(());
    }
    Address::parse(&format!("0x{}", hex::encode(bytes))).map_err(|_| ())
}

fn bounded_uint(token: Option<&Token>, bits: usize) -> Result<ethabi::ethereum_types::U256, ()> {
    match token {
        Some(Token::Uint(value)) if bits == 256 || value.bits() <= bits => Ok(*value),
        _ => Err(()),
    }
}

fn concrete_amount(token: Option<&Token>) -> Result<Amount, ()> {
    let value = bounded_uint(token, 256)?;
    if value.is_zero() || value.bits() > 128 {
        return Err(());
    }
    Ok(Amount(value.low_u128()))
}

fn token_bool(token: Option<&Token>) -> Result<bool, ()> {
    match token {
        Some(Token::Bool(value)) => Ok(*value),
        _ => Err(()),
    }
}

fn malformed_evidence(
    router_kind: RouterKind,
    selector_kind: OuterSelectorKind,
    wrapper_kind: WrapperKind,
) -> OriginEvidence {
    mark_malformed(OriginEvidence::new(
        router_kind,
        selector_kind,
        wrapper_kind,
    ))
}

fn mark_malformed(mut evidence: OriginEvidence) -> OriginEvidence {
    evidence.supported = false;
    evidence.unsupported_reason = UnsupportedReason::MalformedCalldata;
    evidence
}

fn unsupported_evidence(
    router_kind: RouterKind,
    selector_kind: OuterSelectorKind,
    wrapper_kind: WrapperKind,
    reason: UnsupportedReason,
    command_count: usize,
) -> OriginEvidence {
    let mut evidence = OriginEvidence::new(router_kind, selector_kind, wrapper_kind);
    evidence.command_count = command_count;
    mark_unsupported(evidence, reason)
}

fn mark_unsupported(mut evidence: OriginEvidence, reason: UnsupportedReason) -> OriginEvidence {
    evidence.supported = false;
    evidence.exact_in = match reason {
        UnsupportedReason::ExactOutput => Some(false),
        UnsupportedReason::AmbiguousMultiSwap => None,
        _ => evidence.exact_in,
    };
    evidence.unsupported_reason = reason;
    evidence
}

fn selectors() -> &'static Selectors {
    SELECTORS.get_or_init(|| Selectors {
        legacy_exact_input_single: ethabi::short_signature(
            "exactInputSingle",
            &[legacy_exact_input_single_type()],
        ),
        legacy_exact_input: ethabi::short_signature(
            "exactInput",
            &[ParamType::Tuple(vec![
                ParamType::Bytes,
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Uint(256),
                ParamType::Uint(256),
            ])],
        ),
        legacy_exact_output_single: ethabi::short_signature(
            "exactOutputSingle",
            &[legacy_exact_input_single_type()],
        ),
        legacy_exact_output: ethabi::short_signature(
            "exactOutput",
            &[ParamType::Tuple(vec![
                ParamType::Bytes,
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Uint(256),
                ParamType::Uint(256),
            ])],
        ),
        legacy_multicall: ethabi::short_signature(
            "multicall",
            &[ParamType::Array(Box::new(ParamType::Bytes))],
        ),
        router02_exact_input_single: ethabi::short_signature(
            "exactInputSingle",
            &[router02_exact_input_single_type()],
        ),
        universal_execute: ethabi::short_signature(
            "execute",
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
            ],
        ),
        universal_execute_deadline: ethabi::short_signature(
            "execute",
            &[
                ParamType::Bytes,
                ParamType::Array(Box::new(ParamType::Bytes)),
                ParamType::Uint(256),
            ],
        ),
        refund_eth: ethabi::short_signature("refundETH", &[]),
        unwrap_weth9: ethabi::short_signature(
            "unwrapWETH9",
            &[ParamType::Uint(256), ParamType::Address],
        ),
        sweep_token: ethabi::short_signature(
            "sweepToken",
            &[ParamType::Address, ParamType::Uint(256), ParamType::Address],
        ),
        unwrap_weth9_with_fee: ethabi::short_signature(
            "unwrapWETH9WithFee",
            &[
                ParamType::Uint(256),
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
            ],
        ),
        sweep_token_with_fee: ethabi::short_signature(
            "sweepTokenWithFee",
            &[
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
                ParamType::Uint(256),
                ParamType::Address,
            ],
        ),
        self_permit: self_permit_selector("selfPermit"),
        self_permit_if_necessary: self_permit_selector("selfPermitIfNecessary"),
        self_permit_allowed: self_permit_selector("selfPermitAllowed"),
        self_permit_allowed_if_necessary: self_permit_selector("selfPermitAllowedIfNecessary"),
    })
}

fn self_permit_selector(name: &str) -> [u8; 4] {
    ethabi::short_signature(
        name,
        &[
            ParamType::Address,
            ParamType::Uint(256),
            ParamType::Uint(256),
            ParamType::Uint(8),
            ParamType::FixedBytes(32),
            ParamType::FixedBytes(32),
        ],
    )
}

fn legacy_exact_input_single_type() -> ParamType {
    ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ])
}

fn router02_exact_input_single_type() -> ParamType {
    ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(24),
        ParamType::Address,
        ParamType::Uint(256),
        ParamType::Uint(256),
        ParamType::Uint(160),
    ])
}

fn permit_detail_type() -> ParamType {
    ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Uint(160),
        ParamType::Uint(48),
        ParamType::Uint(48),
    ])
}

fn permit_single_type() -> ParamType {
    ParamType::Tuple(vec![
        permit_detail_type(),
        ParamType::Address,
        ParamType::Uint(256),
    ])
}

fn permit_batch_type() -> ParamType {
    ParamType::Tuple(vec![
        ParamType::Array(Box::new(permit_detail_type())),
        ParamType::Address,
        ParamType::Uint(256),
    ])
}

fn allowance_transfer_detail_type() -> ParamType {
    ParamType::Tuple(vec![
        ParamType::Address,
        ParamType::Address,
        ParamType::Uint(160),
        ParamType::Address,
    ])
}
