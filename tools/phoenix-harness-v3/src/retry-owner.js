/**
 * Single retry owner (V3 reliability hardening, 2026-08-24).
 *
 * Canonical Phoenix V3 intent: request retry is owned INSIDE the phoenix
 * preset by the tiered transport retry policy
 * (src/transport-retry.js: routine=2 / normal=3 / critical=6, with the
 * TRANSPORT class override normal=8 on the bounded schedule
 * 500ms..30s). The harness's flat/generic provider retry policy
 * (`@deepseek-ai/dsh-llm-retry`, default normal mode = 2 retries) must NOT
 * compete with or pre-empt it — one failure must produce retry events from
 * ONE policy owner only.
 *
 * Mechanism (per @deepseek-ai/dsh-llm-retry semantics):
 *  - adapter-owned nested `retryPolicy` overrides the flat default;
 *  - `dsh-llm-deepseek` carries `retryPolicy` at the adapter config level →
 *    disabled via the canonical host composition patch
 *    (presets/phoenix-v3/host-cordis.patch.yml -> ~/.dsh/profiles/web/cordis.patch.yml);
 *  - multi-provider adapters (`dsh-llm-pi-ai`) place `retryPolicy` inside
 *    EACH provider profile in $DSH_HOME/settings.yaml → disabled for every
 *    configured provider via disableFlatRetryForPiAi(), applied by the
 *    canonical installer (bin/phoenix-harness-v3.mjs).
 *
 * maxRetries 0 in normal mode means "no retries, delegate downstream" — the
 * Phoenix plugin then remains the only `{kind:'retry'}` producer.
 * Authentication / quota / invalid request remain non-retryable everywhere:
 * they are outside RETRYABLE_CODES in BOTH policies.
 *
 * Pure functions only — no fs, no process. Unit-testable without a harness.
 */

/** Same five codes as the Phoenix tiered policy and the harness default. */
export const FLAT_RETRYABLE_CODES = ['EMPTY_RESPONSE', 'RATE_LIMIT', 'SERVER', 'TIMEOUT', 'TRANSPORT']

/** The exact flat-policy shape that turns generic provider retries OFF. */
export function flatRetryOffPolicy() {
  return {
    mode: 'normal',
    maxRetries: 0,
    retryableCodes: [...FLAT_RETRYABLE_CODES],
  }
}

function sameCodes(a, b) {
  return Array.isArray(a) && a.length === b.length && b.every((c) => a.includes(c))
}

/** True when a provider profile already disables the flat policy. */
export function isFlatRetryDisabled(retryPolicy) {
  const rp = retryPolicy
  if (!rp || typeof rp !== 'object') return false
  if (String(rp.mode ?? '') !== 'normal') return false
  if (Number(rp.maxRetries) !== 0) return false
  if (!sameCodes(rp.retryableCodes, FLAT_RETRYABLE_CODES)) return false
  return true
}

/**
 * Disable the flat provider retry policy for EVERY provider profile under
 * settings.yaml `llm-pi-ai.providers` (deepseek-official and openrouter-ox
 * and any future provider alike). Idempotent: already-disabled profiles are
 * left untouched and not reported as changed.
 *
 * @param settingsDoc parsed settings.yaml document (mutated copy-free: the
 *        caller owns serialization)
 * @returns {{doc: object, changed: string[]}} changed = provider ids that
 *          gained the disabled policy
 */
export function disableFlatRetryForPiAi(settingsDoc) {
  const doc = settingsDoc && typeof settingsDoc === 'object' ? settingsDoc : {}
  const providers = doc?.['llm-pi-ai']?.providers
  const changed = []
  if (providers && typeof providers === 'object') {
    for (const [id, profile] of Object.entries(providers)) {
      if (!profile || typeof profile !== 'object') continue
      if (isFlatRetryDisabled(profile.retryPolicy)) continue
      profile.retryPolicy = flatRetryOffPolicy()
      changed.push(id)
    }
  }
  return { doc, changed }
}
