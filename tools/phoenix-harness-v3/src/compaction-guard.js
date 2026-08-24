/**
 * Compaction circuit breaker + adaptive summary capacity (V3 quickfix).
 *
 * Defect: compaction summarization repeatedly failed (summary truncation at
 * the 6K token cap, DeepSeek TRANSPORT failures) and the harness retried on
 * every following model step, burning wall-clock. A failed compaction must
 * never block normal engineering.
 *
 * Fix, implemented as a guard around the summarization `llm/stream` call
 * (identified by options.purpose === 'compaction'):
 *  - adaptive summary capacity: maxTokens scales with the source surface
 *    (floor 6K, cap 16K) so real Phoenix sessions get room and hidden
 *    reasoning tokens stop truncating the summary;
 *  - circuit breaker: N consecutive summarization failures OPEN the breaker;
 *    while open every summarization attempt fails instantly (no provider
 *    call) so dsh-compaction-basic falls back to its pruned/full surface —
 *    the prune-only fallback — without transport thrash;
 *  - cooldown with exponential backoff (5 min base, doubling, 30 min cap);
 *    after the cooldown ONE probe attempt (half-open) runs: success closes
 *    the breaker, failure re-opens it with the doubled cooldown.
 *
 * The breaker NEVER blocks engineering: it only throws a controlled error
 * from the summarization stream, which dsh-compaction-basic treats as a
 * summarization failure (warn + continue the turn).
 */

const COMPACTION_PURPOSE = 'compaction'
export const BREAKER_CODE = 'PHOENIX_COMPACTION_BREAKER_OPEN'

export const DEFAULTS = {
  tripThreshold: 2, // consecutive summarization failures that open the breaker
  cooldownMs: 5 * 60_000, // base cooldown
  maxCooldownMs: 30 * 60_000, // cooldown cap after doubling
  maxTokensFloor: 6144, // never below the calibrated 6K floor
  maxTokensCap: 16384, // never above 16K (Flash summarizer output ceiling)
  charsPerToken: 3.2, // adaptive slope: ~0.31 tokens per source char
}

function clamp(n, lo, hi) {
  return Math.min(Math.max(n, lo), hi)
}

export function adaptiveMaxTokens(currentMaxTokens, estSourceChars, cfg = DEFAULTS) {
  const base = Number(currentMaxTokens) > 0 ? Number(currentMaxTokens) : cfg.maxTokensFloor
  const target = Math.ceil((estSourceChars || 0) / cfg.charsPerToken)
  return clamp(Math.max(base, target), cfg.maxTokensFloor, cfg.maxTokensCap)
}

/**
 * Per-session compaction guard.
 *
 * API:
 *  - gate(sid, estSourceChars, currentMaxTokens) -> {maxTokens} | throws
 *      (throw = breaker open; skip the provider call entirely)
 *  - noteSuccess(sid)  — stream finished OK
 *  - noteFailure(sid)  — stream errored / aborted / hit max-tokens
 *  - stats(sid)        — {attempts, failures, skips, trips, phase,
 *                        cooldownUntilMs, lastError, everAttempted}
 */
export function createCompactionGuard(config = {}) {
  const cfg = { ...DEFAULTS, ...config }
  const states = new Map() // sid -> state
  const MAX_TRACKED = 32

  function state(sid) {
    const key = String(sid ?? 'unknown')
    let s = states.get(key)
    if (!s) {
      s = {
        phase: 'closed',
        failures: 0,
        openedAt: 0,
        cooldownMs: cfg.cooldownMs,
        attempts: 0,
        failuresTotal: 0,
        skips: 0,
        trips: 0,
        lastError: null,
        everAttempted: false,
      }
      states.set(key, s)
      if (states.size > MAX_TRACKED) states.delete(states.keys().next().value)
    }
    return s
  }

  function gate(sid, estSourceChars, currentMaxTokens) {
    const s = state(sid)
    const now = Date.now()
    if (s.phase === 'open') {
      if (now - s.openedAt < s.cooldownMs) {
        s.skips += 1
        const err = new Error(
          `compaction breaker open (${s.failuresTotal} failures; tripped ${s.trips}x) — prune-only fallback until ${new Date(s.openedAt + s.cooldownMs).toISOString()}`
        )
        err.code = BREAKER_CODE
        throw err
      }
      s.phase = 'half-open' // cooldown elapsed: allow exactly one probe
    }
    s.attempts += 1
    s.everAttempted = true
    return { maxTokens: adaptiveMaxTokens(currentMaxTokens, estSourceChars, cfg) }
  }

  function noteSuccess(sid) {
    const s = state(sid)
    s.phase = 'closed'
    s.failures = 0
    s.cooldownMs = cfg.cooldownMs
    s.lastError = null
  }

  function noteFailure(sid, err) {
    const s = state(sid)
    s.failures += 1
    s.failuresTotal += 1
    s.lastError = err ? String(err?.message ?? err).slice(0, 200) : null
    const reopen = s.phase === 'half-open' || (s.phase === 'closed' && s.failures >= cfg.tripThreshold)
    if (reopen) {
      const failedProbe = s.phase === 'half-open'
      s.phase = 'open'
      s.openedAt = Date.now()
      s.trips += 1
      // the FIRST trip uses the base cooldown; only failed half-open probes
      // double it (exponential backoff up to the cap)
      if (failedProbe) s.cooldownMs = Math.min(s.cooldownMs * 2, cfg.maxCooldownMs)
    }
  }

  function stats(sid) {
    const s = state(sid)
    const openUntil = s.phase === 'open' ? s.openedAt + s.cooldownMs : 0
    return {
      phase: s.phase,
      attempts: s.attempts,
      failures: s.failuresTotal,
      skips: s.skips,
      trips: s.trips,
      cooldownMs: s.cooldownMs,
      cooldownUntilMs: openUntil,
      lastError: s.lastError,
      everAttempted: s.everAttempted,
    }
  }

  return { gate, noteSuccess, noteFailure, stats, purpose: COMPACTION_PURPOSE, cfg }
}
