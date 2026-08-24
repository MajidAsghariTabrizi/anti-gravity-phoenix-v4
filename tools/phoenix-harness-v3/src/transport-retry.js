/**
 * Tiered transport retry policy (V3 quickfix — request-level retry storm
 * recovery). Deterministic budgets per reasoning tier:
 *
 *   routine  = 2 retries (mechanical)
 *   normal   = 3 retries (standard)
 *   critical = 6 retries
 *
 * TRANSPORT hotfix (2026-08-23): TRANSPORT failures on normal-tier model
 * requests carry a class override of 8 retries with the bounded schedule
 * [500ms, 1s, 2s, 4s, 8s, 12s, 20s, 30s] (existing ±jitter applies). Every
 * other retryable code (EMPTY_RESPONSE, RATE_LIMIT, SERVER, TIMEOUT) keeps
 * the tier budget, and routine/critical tiers keep their existing TRANSPORT
 * budgets. Schedule entries are explicit policy constants and are NOT
 * capped by maxDelayMs.
 *
 * Pure policy decisions live here (unit-testable without a harness); the
 * wiring in plugin.js listens on the `agent/request-error` waterfall, counts
 * durable `llm/retry` events for the same (turn, step), and returns
 * {kind:'retry'} after a bounded backoff — the same contract as the
 * harness `dsh-llm-retry` provider policy, but tiered per request class.
 *
 * The retryable set mirrors the harness provider retry classification:
 * EMPTY_RESPONSE, RATE_LIMIT, SERVER, TIMEOUT, TRANSPORT. Only these codes
 * are ever retried; authentication/quota/invalid-request/protocol failures
 * always fall through (fail closed — never blind-retry).
 */

export const RETRYABLE_CODES = ['EMPTY_RESPONSE', 'RATE_LIMIT', 'SERVER', 'TIMEOUT', 'TRANSPORT']
export const POLICY_KEY = 'phoenix-v3-tiered-transport-v1'

export const TIER_BUDGETS = { routine: 2, normal: 3, critical: 6 }

/** Per-code class overrides on top of the tier budget (TRANSPORT hotfix). */
export const CLASS_BUDGET_OVERRIDES = { TRANSPORT: { normal: 8 } }

export const DEFAULTS = {
  budgets: { ...TIER_BUDGETS },
  initialDelayMs: 500,
  maxDelayMs: 10000,
  jitterRatio: 0.1,
  transportBackoffScheduleMs: [500, 1000, 2000, 4000, 8000, 12000, 20000, 30000],
}

/** Map the reasoning-tier label to the retry-tier label. */
export function tierOf(reasoningLabel) {
  const label = String(reasoningLabel ?? 'standard').toLowerCase()
  if (label === 'mechanical') return 'routine'
  if (label === 'critical') return 'critical'
  return 'normal' // standard / unknown
}

/** Retry budget for a reasoning-tier label (never negative, capped at 12). */
export function budgetFor(reasoningLabel, config = {}) {
  const tier = tierOf(reasoningLabel)
  const budgets = config.budgets ?? config
  const raw = Number(budgets?.[tier] ?? TIER_BUDGETS[tier])
  if (!Number.isFinite(raw)) return TIER_BUDGETS[tier]
  return Math.min(Math.max(Math.round(raw), 0), 12)
}

/**
 * Retry budget for a failure code: applies the class override (TRANSPORT →
 * normal 8) on top of the tier budget; every other code uses the tier
 * budget unchanged. Never negative, capped at 12.
 */
export function budgetForCode(reasoningLabel, code, config = {}) {
  const tier = tierOf(reasoningLabel)
  const cfg = config.budgets ?? config
  const base = Number(cfg?.[tier] ?? TIER_BUDGETS[tier])
  const fallback = Number.isFinite(base) ? base : TIER_BUDGETS[tier]
  const overrides = config.classOverrides ?? CLASS_BUDGET_OVERRIDES
  const raw = overrides?.[String(code ?? '')]?.[tier] ?? fallback
  const n = Number(raw)
  if (!Number.isFinite(n)) return fallback
  return Math.min(Math.max(Math.round(n), 0), 12)
}

export function isRetryable(code) {
  return RETRYABLE_CODES.includes(String(code ?? ''))
}

/**
 * One retry decision.
 * @param code         failure code from agent/request-error payload
 * @param priorRetries durable retries already recorded for this (turn, step)
 * @param budget       budget (retries, not attempts) for this code/tier
 * @returns {{retry: boolean, reason: string, budget: number}}
 */
export function decide(code, priorRetries, budget) {
  const b = Math.max(Math.round(Number(budget) || 0), 0)
  if (b === 0) return { retry: false, reason: 'no-budget', budget: b }
  if (!isRetryable(code)) return { retry: false, reason: 'not-retryable', budget: b }
  const prior = Math.max(Math.round(Number(priorRetries) || 0), 0)
  if (prior >= b) return { retry: false, reason: 'budget-exhausted', budget: b }
  return { retry: true, reason: 'retry', budget: b }
}

/**
 * Bounded backoff. TRANSPORT failures (code='TRANSPORT') follow the explicit
 * transportBackoffScheduleMs (500ms → 30s, jittered, not capped by
 * maxDelayMs); every other code keeps the bounded exponential backoff
 * (initialDelayMs * 2^(retry-1), capped at maxDelayMs), mirroring the
 * harness provider policy. A valid providerRetryAfterMs at or below
 * maxDelayMs replaces local backoff.
 */
export function delayMs(retry, config = {}, providerRetryAfterMs, random = Math.random, code = '') {
  const cfg = { ...DEFAULTS, ...config }
  const n = Math.max(Math.round(Number(retry) || 1), 1)
  if (Number.isFinite(providerRetryAfterMs) && providerRetryAfterMs > 0 && providerRetryAfterMs <= cfg.maxDelayMs) {
    return providerRetryAfterMs
  }
  const schedule = String(code ?? '') === 'TRANSPORT' && Array.isArray(cfg.transportBackoffScheduleMs)
    ? cfg.transportBackoffScheduleMs
    : null
  if (schedule && n <= schedule.length) {
    const base = Math.max(Number(schedule[n - 1]) || 0, 0)
    const jitter = 1 - cfg.jitterRatio + 2 * cfg.jitterRatio * random()
    return Math.max(Math.round(base * jitter), 0)
  }
  const exponent = Math.min(n - 1, 1024)
  const exponential = Math.min(cfg.initialDelayMs * 2 ** exponent, cfg.maxDelayMs)
  const jitter = 1 - cfg.jitterRatio + 2 * cfg.jitterRatio * random()
  return Math.min(Math.max(Math.round(exponential * jitter), 0), cfg.maxDelayMs)
}

/**
 * Count durable retries already recorded by THIS policy for the failed
 * (turn, step) — the harness counts per turn/step so retried attempts keep
 * their identity. Events lacking turn/step (older records) are ignored.
 */
export function countPriorRetries(events, turn, step) {
  if (!Array.isArray(events)) return 0
  let prior = 0
  for (const event of events) {
    if (event?.type !== 'llm/retry') continue
    const d = event.data
    if (d?.policyKey !== POLICY_KEY) continue
    if (d.turn !== turn || d.step !== step) continue
    if (Number(d.retry) > prior) prior = Number(d.retry)
  }
  return prior
}

/** RetryId of the newest prior event for this (turn, step), if any. */
export function priorRetryId(events, turn, step) {
  if (!Array.isArray(events)) return null
  let id = null
  for (const event of events) {
    if (event?.type !== 'llm/retry') continue
    const d = event.data
    if (d?.policyKey !== POLICY_KEY) continue
    if (d.turn !== turn || d.step !== step) continue
    if (typeof d.retryId === 'string') id = d.retryId
  }
  return id
}
