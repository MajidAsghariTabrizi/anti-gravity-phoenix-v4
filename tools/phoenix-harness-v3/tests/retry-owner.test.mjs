/**
 * Focused reliability tests — SINGLE RETRY OWNER (V3 reliability hardening,
 * 2026-08-24). Scenarios:
 *   11    DeepSeek mocked TRANSPORT   -> exactly one Phoenix retry owner
 *   12    Ox Alpha mocked TRANSPORT   -> exactly one Phoenix retry owner
 *   13    normal TRANSPORT            -> Phoenix budget 8
 *   14    SERVER/TIMEOUT/RATE_LIMIT   -> normal budget stays 3
 *   15    no duplicate llm/retry events for one attempt (single identity)
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  POLICY_KEY, RETRYABLE_CODES, TIER_BUDGETS, CLASS_BUDGET_OVERRIDES,
  budgetForCode, decide, delayMs, countPriorRetries, priorRetryId,
} from '../src/transport-retry.js'
import {
  FLAT_RETRYABLE_CODES, flatRetryOffPolicy, isFlatRetryDisabled, disableFlatRetryForPiAi,
} from '../src/retry-owner.js'

const here = dirname(fileURLToPath(import.meta.url))
const HOST_PATCH = join(here, '..', 'presets', 'phoenix-v3', 'host-cordis.patch.yml')

/** Mocked TRANSPORT storm driven ONLY by the Phoenix policy (one owner). */
function mockedTransportStorm(budget, foreignEvents = []) {
  const events = [...foreignEvents]
  const retryIds = []
  for (;;) {
    const turn = 7
    const step = 3
    const prior = countPriorRetries(events, turn, step)
    const decision = decide('TRANSPORT', prior, budget)
    if (!decision.retry) return { events, exhausted: decision.reason === 'budget-exhausted', retryIds }
    const retry = prior + 1
    const retryId = priorRetryId(events, turn, step) ?? `rid-${retryIds.length + 1}`
    retryIds.push(retryId)
    events.push({ type: 'llm/retry', data: { policyKey: POLICY_KEY, turn, step, retry, retryId, maxRetries: budget, failure: { code: 'TRANSPORT' } } })
  }
}

test('scenario 13: normal TRANSPORT -> Phoenix budget 8 (bounded schedule preserved)', () => {
  assert.equal(budgetForCode('standard', 'TRANSPORT'), 8)
  assert.deepEqual(TIER_BUDGETS, { routine: 2, normal: 3, critical: 6 })
  assert.deepEqual(CLASS_BUDGET_OVERRIDES, { TRANSPORT: { normal: 8 } })
  // schedule entries are explicit constants, not capped by maxDelayMs
  const d = [500, 1000, 2000, 4000, 8000, 12000, 20000, 30000]
  d.forEach((ms, i) => assert.equal(delayMs(i + 1, {}, undefined, () => 0.5, 'TRANSPORT'), ms))
})

test('scenario 14: SERVER/TIMEOUT/RATE_LIMIT keep normal=3; AUTH/quota/invalid stay non-retryable', () => {
  for (const code of ['SERVER', 'TIMEOUT', 'RATE_LIMIT']) {
    assert.equal(budgetForCode('standard', code), 3, code)
  }
  assert.equal(decide('AUTH', 0, 6).retry, false)
  assert.equal(decide('QUOTA', 0, 6).retry, false)
  assert.equal(decide('INVALID_REQUEST', 0, 6).retry, false)
  assert.deepEqual(RETRYABLE_CODES.sort(), ['EMPTY_RESPONSE', 'RATE_LIMIT', 'SERVER', 'TIMEOUT', 'TRANSPORT'].sort())
})

test('scenario 11: DeepSeek — flat policy disabled at the adapter level (canonical host patch); one Phoenix owner', () => {
  const patch = readFileSync(HOST_PATCH, 'utf8')
  assert.match(patch, /id:\s*llm-deepseek/, 'canonical host patch targets the DeepSeek adapter row')
  assert.match(patch, /maxRetries:\s*0/, 'flat retry disabled (maxRetries 0)')
  assert.match(patch, /mode:\s*normal/, 'normal mode keeps policy identity stable')
  for (const c of FLAT_RETRYABLE_CODES) assert.ok(patch.includes(c), `code set declared: ${c}`)

  // With the competing owner OFF, one mocked TRANSPORT failure produces
  // retry events from the Phoenix policy ONLY: budget 8, one identity chain.
  const { exhausted, retryIds } = mockedTransportStorm(8)
  assert.ok(exhausted)
  assert.equal(retryIds.length, 8)
  assert.equal(new Set(retryIds).size, 1, 'all retries share ONE retry identity (single owner, single chain)')
})

test('scenario 12: Ox Alpha (openrouter-ox pi-ai profile) — flat policy disabled per provider; one Phoenix owner', () => {
  const doc = {
    'agent-presets': { default: 'phoenix' },
    'llm-pi-ai': { providers: { 'openrouter-ox': { displayName: 'OpenRouter Ox Alpha', apiKeyEnv: 'X', models: [{ id: 'stealth/ox-alpha' }] } } },
  }
  const { changed } = disableFlatRetryForPiAi(doc)
  assert.deepEqual(changed, ['openrouter-ox'])
  const rp = doc['llm-pi-ai'].providers['openrouter-ox'].retryPolicy
  assert.ok(isFlatRetryDisabled(rp))
  assert.equal(rp.maxRetries, 0)
  assert.equal(rp.mode, 'normal')
  assert.equal(doc['llm-pi-ai'].providers['openrouter-ox'].displayName, 'OpenRouter Ox Alpha')
  assert.equal(doc['agent-presets'].default, 'phoenix')
  // idempotent reinstall changes nothing further
  const again = disableFlatRetryForPiAi(doc)
  assert.deepEqual(again.changed, [])

  // One mocked TRANSPORT storm under the Phoenix policy only — no flat-policy
  // events ever appear alongside (foreign policyKey would be ignored anyway).
  const foreign = [
    { type: 'llm/retry', data: { policyKey: 'flat-generic-v1', turn: 7, step: 3, retry: 1 } },
    { type: 'llm/retry', data: { policyKey: 'flat-generic-v1', turn: 7, step: 3, retry: 2 } },
  ]
  assert.equal(countPriorRetries(foreign, 7, 3), 0, 'foreign retries must not consume the Phoenix budget')
  const { exhausted, retryIds } = mockedTransportStorm(8, foreign)
  assert.ok(exhausted)
  assert.equal(new Set(retryIds).size, 1)
})

test('scenario 15: one attempt -> exactly one new llm/retry event, reused identity, correct exhaustion', () => {
  const turn = 2
  const step = 5
  const events = []
  let scheduled = 0
  const budget = budgetForCode('standard', 'TRANSPORT')
  for (;;) {
    const prior = countPriorRetries(events, turn, step)
    const decision = decide('TRANSPORT', prior, budget)
    if (!decision.retry) break
    const retryId = priorRetryId(events, turn, step) ?? 'rid-A'
    events.push({ type: 'llm/retry', data: { policyKey: POLICY_KEY, turn, step, retry: prior + 1, retryId, failure: { code: 'TRANSPORT' } } })
    scheduled += 1
    // duplicate append guard: same retry number twice must NOT grow prior count
    events.push({ type: 'llm/retry', data: { policyKey: POLICY_KEY, turn, step, retry: prior + 1, retryId, duplicate: true, failure: { code: 'TRANSPORT' } } })
  }
  assert.equal(scheduled, budget, 'exactly budget-many scheduling decisions (duplicates do not advance)')
  assert.equal(countPriorRetries(events, turn, step), budget)
  assert.equal(priorRetryId(events, turn, step), 'rid-A')
  assert.equal(countPriorRetries(events, turn, step + 1), 0, 'other steps untouched')
})

test('flatRetryOffPolicy shape: normal/maxRetries 0/codes declared (delegate, never blind-retry)', () => {
  const rp = flatRetryOffPolicy()
  assert.ok(isFlatRetryDisabled(rp))
  assert.equal(rp.retryableCodes.length, FLAT_RETRYABLE_CODES.length)
  assert.notEqual(rp.mode, 'always', 'always-mode would retry permanent failures — forbidden here')
})
