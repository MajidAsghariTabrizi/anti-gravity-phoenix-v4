use crate::economic_control::{
    EvidenceGate, ReadinessBinding, SizeLevel, MAXIMUM_CANDIDATE_AGE_MS,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const ACTIVATION_REQUEST_SCHEMA: &str = "phoenix.economic-activation-request.v1";
pub const ACTIVATION_REQUEST_TTL_SECONDS: i64 = 60;
pub const MAX_ACTIVATION_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationCandidateEvidence {
    pub candidate_id: Uuid,
    pub candidate_hash: String,
    pub candidate_plan_hash: String,
    pub fork_plan_hash: String,
    pub fork_result_hash: String,
    pub route_fingerprint: String,
    pub route_policy_hash: String,
    pub state_block_number: u64,
    pub state_block_hash: String,
    pub state_hash: String,
    pub executor_address: String,
    pub executor_code_hash: String,
    pub selected_size_wei: u128,
    pub predicted_gross_profit_wei: u128,
    pub predicted_total_cost_wei: u128,
    pub conservative_predicted_net_pnl_wei: i128,
    pub fork_simulated_net_pnl_wei: i128,
    pub fork_simulated_gas_cost_wei: u128,
    pub candidate_created_at: DateTime<Utc>,
    pub candidate_expires_at: DateTime<Utc>,
    pub fork_simulated_at: DateTime<Utc>,
}

impl ActivationCandidateEvidence {
    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), ActivationRequestError> {
        if !canonical_hex(&self.candidate_hash, 64)
            || !canonical_hex(&self.candidate_plan_hash, 64)
            || !canonical_hex(&self.fork_plan_hash, 64)
            || !canonical_hex(&self.fork_result_hash, 64)
            || !canonical_hex(&self.route_policy_hash, 64)
            || !canonical_hash(&self.state_block_hash)
            || !canonical_hex(&self.state_hash, 64)
            || !canonical_address(&self.executor_address)
            || !canonical_hex(&self.executor_code_hash, 64)
            || self.route_fingerprint.is_empty()
            || self.route_fingerprint.len() > 256
            || self.state_block_number == 0
            || self.selected_size_wei != SizeLevel::Min.amount_wei()
            || self.predicted_gross_profit_wei <= self.predicted_total_cost_wei
            || self.conservative_predicted_net_pnl_wei <= 0
            || self.fork_simulated_net_pnl_wei <= 0
            || self.fork_simulated_gas_cost_wei == 0
        {
            return Err(ActivationRequestError::Candidate);
        }
        if self.candidate_created_at >= self.candidate_expires_at
            || now >= self.candidate_expires_at
            || self.candidate_created_at > now
            || self.fork_simulated_at > now
            || nonnegative_milliseconds(now.signed_duration_since(self.candidate_created_at))
                > MAXIMUM_CANDIDATE_AGE_MS
            || nonnegative_milliseconds(now.signed_duration_since(self.fork_simulated_at))
                > MAXIMUM_CANDIDATE_AGE_MS
        {
            return Err(ActivationRequestError::Stale);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationRequest {
    pub schema_version: String,
    pub request_id: Uuid,
    pub binding: ReadinessBinding,
    pub evidence: EvidenceGate,
    pub candidate: ActivationCandidateEvidence,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub request_hash: String,
}

impl ActivationRequest {
    pub fn seal(&mut self) -> Result<(), ActivationRequestError> {
        self.request_hash = canonical_contract_hash(
            &serde_json::to_value(&*self).map_err(|_| ActivationRequestError::Contract)?,
            "request_hash",
            "economic-activation-request",
            ACTIVATION_REQUEST_SCHEMA,
        )?;
        Ok(())
    }

    pub fn validate(&self, now: DateTime<Utc>) -> Result<(), ActivationRequestError> {
        if self.schema_version != ACTIVATION_REQUEST_SCHEMA
            || !canonical_hex(&self.request_hash, 64)
            || self.created_at >= self.expires_at
            || self.expires_at - self.created_at > Duration::seconds(ACTIVATION_REQUEST_TTL_SECONDS)
            || now >= self.expires_at
        {
            return Err(ActivationRequestError::Contract);
        }
        self.binding
            .validate(now)
            .map_err(|_| ActivationRequestError::Binding)?;
        self.evidence
            .validate()
            .map_err(|_| ActivationRequestError::Evidence)?;
        self.candidate.validate(now)?;
        if self.binding.route_fingerprint != self.candidate.route_fingerprint
            || self.binding.route_policy_hash != self.candidate.route_policy_hash
            || self.binding.executor_code_hash != self.candidate.executor_code_hash
            || self.binding.candidate_evidence_hashes
                != [
                    self.candidate.candidate_hash.clone(),
                    self.candidate.fork_result_hash.clone(),
                ]
            || self.created_at != self.binding.created_at
            || self.expires_at != self.binding.expires_at
        {
            return Err(ActivationRequestError::Binding);
        }
        let value = serde_json::to_value(self).map_err(|_| ActivationRequestError::Contract)?;
        let expected = canonical_contract_hash(
            &value,
            "request_hash",
            "economic-activation-request",
            ACTIVATION_REQUEST_SCHEMA,
        )?;
        if self.request_hash != expected {
            return Err(ActivationRequestError::Hash);
        }
        Ok(())
    }
}

pub fn canonical_contract_hash(
    value: &Value,
    field: &str,
    domain: &str,
    schema: &str,
) -> Result<String, ActivationRequestError> {
    let mut body = value.clone();
    body.as_object_mut()
        .ok_or(ActivationRequestError::Contract)?
        .remove(field)
        .ok_or(ActivationRequestError::Contract)?;
    let prefix = format!("phoenix.canonical-json.v1:{domain}:{schema}\n");
    Ok(hex::encode(Sha256::digest(
        [prefix.as_bytes(), canonical_json(&body)?.as_slice()].concat(),
    )))
}

pub fn canonical_domain_hash(
    domain: &str,
    value: &Value,
) -> Result<String, ActivationRequestError> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(b'\n');
    bytes.extend(canonical_json(value)?);
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn canonical_json(value: &Value) -> Result<Vec<u8>, ActivationRequestError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Number(_) => {
            serde_json::to_vec(value).map_err(|_| ActivationRequestError::Contract)
        }
        Value::Array(values) => {
            let mut output = vec![b'['];
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend(canonical_json(child)?);
            }
            output.push(b']');
            Ok(output)
        }
        Value::Object(values) => {
            let sorted = values.iter().collect::<BTreeMap<_, _>>();
            let mut output = vec![b'{'];
            for (index, (key, child)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output
                    .extend(serde_json::to_vec(key).map_err(|_| ActivationRequestError::Contract)?);
                output.push(b':');
                output.extend(canonical_json(child)?);
            }
            output.push(b'}');
            Ok(output)
        }
    }
}

#[cfg(unix)]
pub fn write_atomic_request(
    outbox: &Path,
    request: &ActivationRequest,
) -> Result<PathBuf, ActivationRequestError> {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let directory = fs::symlink_metadata(outbox).map_err(|_| ActivationRequestError::Filesystem)?;
    if !directory.file_type().is_dir()
        || directory.file_type().is_symlink()
        || directory.uid() != unsafe { libc::geteuid() }
        || directory.gid() != unsafe { libc::getegid() }
        || directory.mode() & 0o777 != 0o700
    {
        return Err(ActivationRequestError::Filesystem);
    }
    let value = serde_json::to_value(request).map_err(|_| ActivationRequestError::Contract)?;
    let mut bytes = canonical_json(&value)?;
    bytes.push(b'\n');
    if bytes.is_empty() || bytes.len() > MAX_ACTIVATION_REQUEST_BYTES {
        return Err(ActivationRequestError::Contract);
    }
    let stem = format!("activation-request-{}", request.request_id);
    let temporary = outbox.join(format!(".{stem}.tmp"));
    let destination = outbox.join(format!("{stem}.json"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|_| ActivationRequestError::Filesystem)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| ActivationRequestError::Filesystem)?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        let _ = fs::remove_file(&temporary);
        return Err(ActivationRequestError::Filesystem);
    }
    drop(file);
    if destination.exists() {
        let _ = fs::remove_file(&temporary);
        return Err(ActivationRequestError::Filesystem);
    }
    fs::rename(&temporary, &destination).map_err(|_| ActivationRequestError::Filesystem)?;
    fs::File::open(outbox)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ActivationRequestError::Filesystem)?;
    let metadata =
        fs::symlink_metadata(&destination).map_err(|_| ActivationRequestError::Filesystem)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.gid() != unsafe { libc::getegid() }
    {
        let _ = fs::remove_file(&destination);
        return Err(ActivationRequestError::Filesystem);
    }
    Ok(destination)
}

#[cfg(not(unix))]
pub fn write_atomic_request(
    _outbox: &Path,
    _request: &ActivationRequest,
) -> Result<PathBuf, ActivationRequestError> {
    Err(ActivationRequestError::UnsupportedPlatform)
}

fn canonical_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_hash(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|digest| canonical_hex(digest, 64))
}

fn canonical_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|address| canonical_hex(address, 40))
}

fn nonnegative_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.num_milliseconds().max(0)).unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ActivationRequestError {
    #[error("activation request contract is invalid")]
    Contract,
    #[error("activation request hash is invalid")]
    Hash,
    #[error("activation request binding is invalid")]
    Binding,
    #[error("activation request readiness evidence is invalid")]
    Evidence,
    #[error("activation candidate evidence is invalid")]
    Candidate,
    #[error("activation candidate evidence is stale")]
    Stale,
    #[error("activation outbox is unsafe")]
    Filesystem,
    #[error("activation outbox requires Linux")]
    UnsupportedPlatform,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::REVERSE_ROUTE_FINGERPRINT;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_770_000_000, 0)
            .single()
            .expect("valid timestamp")
    }

    fn request() -> ActivationRequest {
        let current = now();
        let candidate = ActivationCandidateEvidence {
            candidate_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").expect("uuid"),
            candidate_hash: "1".repeat(64),
            candidate_plan_hash: "2".repeat(64),
            fork_plan_hash: "3".repeat(64),
            fork_result_hash: "4".repeat(64),
            route_fingerprint: REVERSE_ROUTE_FINGERPRINT.to_string(),
            route_policy_hash: "36da85c0fd07e5d3a12726582b20c84d81cfbd2d1d982da8237d3b5cf38b83d5"
                .to_string(),
            state_block_number: 1,
            state_block_hash: format!("0x{}", "6".repeat(64)),
            state_hash: "7".repeat(64),
            executor_address: format!("0x{}", "8".repeat(40)),
            executor_code_hash: "9".repeat(64),
            selected_size_wei: SizeLevel::Min.amount_wei(),
            predicted_gross_profit_wei: 10,
            predicted_total_cost_wei: 5,
            conservative_predicted_net_pnl_wei: 4,
            fork_simulated_net_pnl_wei: 3,
            fork_simulated_gas_cost_wei: 2,
            candidate_created_at: current - Duration::milliseconds(500),
            candidate_expires_at: current + Duration::seconds(2),
            fork_simulated_at: current - Duration::milliseconds(250),
        };
        let expires_at = current + Duration::seconds(ACTIVATION_REQUEST_TTL_SECONDS);
        let binding = ReadinessBinding {
            release_sha: "a".repeat(40),
            engine_image_digest: format!("sha256:{}", "b".repeat(64)),
            route_fingerprint: candidate.route_fingerprint.clone(),
            route_universe_hash: "84adac686635535486e06e44fcaf90c812dc27273affc5bffc4eebd6c164928c"
                .to_string(),
            route_policy_hash: candidate.route_policy_hash.clone(),
            risk_policy_hash: "d".repeat(64),
            economic_control_epoch: 1,
            global_control_epoch: 2,
            route_control_epoch: 3,
            executor_code_hash: candidate.executor_code_hash.clone(),
            contract_identity_hash: "e".repeat(64),
            wallet_gas_reserve_wei: 2,
            gas_reserve_floor_wei: 1,
            current_daily_loss_wei: 0,
            daily_loss_limit_wei: 1,
            observed_from: current - Duration::minutes(10),
            observed_until: current - Duration::milliseconds(1),
            created_at: current,
            expires_at,
            candidate_evidence_hashes: vec![
                candidate.candidate_hash.clone(),
                candidate.fork_result_hash.clone(),
            ],
        };
        let evidence = EvidenceGate {
            supported_observations: 100,
            valid_acceptance_bps: 10_000,
            process_fatal_integrity_exits: 0,
            quarantine_progress_proven: true,
            consumer_pending_bounded: true,
            ack_pending_bounded: true,
            stale_outbox_rows: 0,
            primary_rpc_healthy: true,
            maximum_state_age_blocks: 0,
            maximum_quote_age_ms: 500,
            maximum_candidate_age_ms: 500,
            fork_attempts: 1,
            fork_passes: 1,
            prediction_error_bps: 0,
            fork_skips: 0,
            execution_requests: 0,
            active_attempts: 0,
            positive_exact_candidates: 1,
        };
        let mut request = ActivationRequest {
            schema_version: ACTIVATION_REQUEST_SCHEMA.to_string(),
            request_id: Uuid::parse_str("22222222-2222-4222-8222-222222222222").expect("uuid"),
            binding,
            evidence,
            candidate,
            created_at: current,
            expires_at,
            request_hash: "0".repeat(64),
        };
        request.seal().expect("seal");
        request
    }

    #[test]
    fn canonical_fresh_reverse_route_request_passes() {
        let request = request();
        assert_eq!(
            request.candidate.route_fingerprint,
            REVERSE_ROUTE_FINGERPRINT
        );
        assert_eq!(request.binding.route_fingerprint, REVERSE_ROUTE_FINGERPRINT);
        request.validate(now()).expect("valid request");
    }

    #[test]
    fn stale_or_unprofitable_candidate_fails_closed() {
        let mut stale = request();
        stale.candidate.candidate_expires_at = now();
        stale.seal().expect("seal");
        assert_eq!(stale.validate(now()), Err(ActivationRequestError::Stale));

        let mut unprofitable = request();
        unprofitable.candidate.conservative_predicted_net_pnl_wei = 0;
        unprofitable.seal().expect("seal");
        assert_eq!(
            unprofitable.validate(now()),
            Err(ActivationRequestError::Candidate)
        );
    }

    #[test]
    fn binding_or_hash_mutation_fails_closed() {
        let mut wrong_binding = request();
        wrong_binding.binding.route_policy_hash = "f".repeat(64);
        wrong_binding.seal().expect("seal");
        assert_eq!(
            wrong_binding.validate(now()),
            Err(ActivationRequestError::Binding)
        );

        let mut wrong_hash = request();
        wrong_hash.request_hash = "f".repeat(64);
        assert_eq!(
            wrong_hash.validate(now()),
            Err(ActivationRequestError::Hash)
        );
    }
}
