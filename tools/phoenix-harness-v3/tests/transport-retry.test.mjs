/**
 * Tiered transport retry policy tests (V3): routine=2 / normal=3 /
 * critical=6 tier budgets; TRANSPORT class override normal=8 with the
 * 500ms..30s schedule (hotfix); retryable-code gating; durable llm/retry
 * counting; bounded backoff with provider retry-after honoring.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const {
  RETRYABLE_CODES, POLICY_KEY, TIER_BUDGETS, CLASS_BUDGET_OVERRIDES, tierOf,
  budgetFor, budgetForCode, isRetryable, decide, delayMs, countPriorRetries, priorRetryId,
} = await import(pathToFileURL(join(ROOT, 'src', 'transport-retry.js')).href)

test('tier budgets are exactly routine=2 normal=3 critical=6', () => {
  assert.deepEqual(TIER_BUDGETS, { routine: 2, normal: 3, critical: 6 })
  assert.equal(budgetFor('mechanical'), 2)
  assert.equal(budgetFor('standard'), 3)
  assert.equal(budgetFor('critical'), 6)
  assert.equal(budgetFor('unknown'), 3)
  assert.equal(budgetFor('mechanical', { budgets: { routine: 1, normal: 2, critical: 3 } }), 1)
  assert.equal(budgetFor('critical', { budgets: { critical: 99 } }), 12, 'budgets cap at 12')
  assert.equal(budgetFor('mechanical', { budgets: { routine: -5 } }), 0)
  assert.equal(tierOf('MECHANICAL'), 'routine')
  assert.equal(tierOf('critical'), 'critical')
})

test('only the retryable transport-class codes are retried (never blind)', () => {
  assert.ok(RETRYABLE_CODES.includes('TRANSPORT'))
  assert.ok(isRetryable('SERVER'))
  assert.ok(!isRetryable('AUTHENTICATION'))
  assert.ok(!isRetryable('QUOTA'))
  assert.ok(!isRetryable('INVALID_REQUEST'))
  assert.equal(decide('TRANSPORT', 0, 2).retry, true)
  assert.equal(decide('TRANSPORT', 1, 2).retry, true)
  assert.equal(decide('TRANSPORT', 2, 2).retry, false)
  assert.equal(decide('TRANSPORT', 2, 2).reason, 'budget-exhausted')
  assert.equal(decide('AUTHENTICATION', 0, 6).retry, false)
  assert.equal(decide('AUTHENTICATION', 0, 6).reason, 'not-retryable')
  assert.equal(decide('TRANSPORT', 0, 0).retry, false)
})

test('durable llm/retry counting is per (turn, step) and per policy', () => {
  const events = [
    { type: 'llm/retry', data: { policyKey: POLICY_KEY, turn: 1, step: 2, retry: 1, retryId: 'r1' } },
    { type: 'llm/retry', data: { policyKey: POLICY_KEY, turn: 1, step: 2, retry: 2, retryId: 'r1' } },
    { type: 'llm/retry', data: { policyKey: 'other-policy', turn: 1, step: 2, retry: 9 } },
    { type: 'llm/retry', data: { policyKey: POLICY_KEY, turn: 1, step: 3, retry: 5 } },
    { type: 'llm/retry', data: { policyKey: POLICY_KEY, retry: 7 } }, // no turn/step: ignored
  ]
  assert.equal(countPriorRetries(events, 1, 2), 2)
  assert.equal(countPriorRetries(events, 1, 3), 5)
  assert.equal(countPriorRetries(events, 2, 2), 0)
  assert.equal(countPriorRetries(undefined, 1, 2), 0)
  assert.equal(priorRetryId(events, 1, 2), 'r1')
  assert.equal(priorRetryId(events, 2, 2), null)
})

test('backoff is bounded exponential with jitter and honors provider retry-after', () => {
  const cfg = { initialDelayMs: 500, maxDelayMs: 10000, jitterRatio: 0.1 }
  const fixed = () => 0.5
  assert.equal(delayMs(1, cfg, undefined, fixed), 500)
  assert.equal(delayMs(2, cfg, undefined, fixed), 1000)
  assert.equal(delayMs(3, cfg, undefined, fixed), 2000)
  assert.ok(delayMs(30, cfg, undefined, fixed) <= 10000)
  assert.equal(delayMs(2, cfg, 250, fixed), 250, 'in-cap provider retry-after replaces local backoff')
  assert.ok(delayMs(2, cfg, 999999, fixed) <= 10000, 'over-cap provider retry-after falls back to local backoff')
  assert.ok(delayMs(1, cfg, undefined, () => 0.99) <= 550, 'jitter stays within the ±ratio bound')
})

test('TRANSPORT hotfix: class override normal=8, other codes/tiers unchanged', () => {
  assert.deepEqual(CLASS_BUDGET_OVERRIDES, { TRANSPORT: { normal: 8 } })
  assert.equal(budgetForCode('standard', 'TRANSPORT'), 8)
  assert.equal(budgetForCode('mechanical', 'TRANSPORT'), 2)
  assert.equal(budgetForCode('critical', 'TRANSPORT'), 6)
  for (const code of ['SERVER', 'TIMEOUT', 'RATE_LIMIT', 'EMPTY_RESPONSE']) {
    assert.equal(budgetForCode('standard', code), 3, `${code} keeps normal=3`)
    assert.equal(budgetForCode('mechanical', code), 2)
    assert.equal(budgetForCode('critical', code), 6)
  }
})

test('mocked TRANSPORT storm: retries 1..8 allowed, exhaustion fail-closes', () => {
  const budget = budgetForCode('standard', 'TRANSPORT')
  assert.equal(budget, 8)
  const events = []
  const delays = []
  const fixed = () => 0.5
  let failures = 0
  // Mocked transport: every attempt fails with TRANSPORT until the policy
  // exhausts its budget. No real API, no real timers.
  while (true) {
    failures++
    const prior = countPriorRetries(events, 1, 1)
    const decision = decide('TRANSPORT', prior, budget)
    if (!decision.retry) break
    const retry = prior + 1
    delays.push(delayMs(retry, {}, undefined, fixed, 'TRANSPORT'))
    events.push({ type: 'llm/retry', data: { policyKey: POLICY_KEY, turn: 1, step: 1, retry, retryId: 'mock-r1' } })
  }
  assert.equal(failures, 9, 'initial attempt + 8 retried attempts, then fail-close')
  assert.equal(events.length, 8, '8 retries recorded')
  assert.deepEqual(delays, [500, 1000, 2000, 4000, 8000, 12000, 20000, 30000])
  const final = decide('TRANSPORT', 8, budget)
  assert.equal(final.retry, false)
  assert.equal(final.reason, 'budget-exhausted')
})

test('non-TRANSPORT codes stay bounded at normal=3 (exhaustion after 3)', () => {
  for (const code of ['SERVER', 'TIMEOUT', 'RATE_LIMIT', 'EMPTY_RESPONSE']) {
    const budget = budgetForCode('standard', code)
    assert.equal(budget, 3)
    assert.equal(decide(code, 2, budget).retry, true)
    assert.equal(decide(code, 3, budget).retry, false)
    assert.equal(decide(code, 3, budget).reason, 'budget-exhausted')
  }
})

test('TRANSPORT backoff schedule 500ms..30s; non-TRANSPORT exponential unchanged', () => {
  const fixed = () => 0.5
  const expected = [500, 1000, 2000, 4000, 8000, 12000, 20000, 30000]
  for (let i = 1; i <= 8; i++) {
    assert.equal(delayMs(i, {}, undefined, fixed, 'TRANSPORT'), expected[i - 1])
  }
  assert.equal(delayMs(4, {}, undefined, fixed, 'SERVER'), 4000, 'non-transport exponential unchanged')
  assert.equal(delayMs(6, {}, undefined, fixed, 'SERVER'), 10000, 'non-transport cap unchanged')
  assert.equal(delayMs(2, {}, 250, fixed, 'TRANSPORT'), 250, 'provider retry-after still honored for TRANSPORT')
})
