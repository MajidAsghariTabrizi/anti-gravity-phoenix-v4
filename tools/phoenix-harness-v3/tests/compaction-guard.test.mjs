/**
 * Compaction circuit breaker + adaptive summary size tests (V3 quickfix).
 * Verifies: adaptive maxTokens bounds, breaker trip on consecutive
 * failures, instant prune-only skip while open, cooldown -> half-open
 * single probe, success close, failure re-open with doubled cooldown.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const { adaptiveMaxTokens, createCompactionGuard, BREAKER_CODE, DEFAULTS } = await import(pathToFileURL(join(ROOT, 'src', 'compaction-guard.js')).href)

test('adaptive summary size respects floor/cap and scales with source chars', () => {
  assert.equal(adaptiveMaxTokens(6144, 0), 6144, 'empty source stays at the floor')
  assert.equal(adaptiveMaxTokens(6144, 10000), 6144, 'small source stays at the floor')
  const mid = adaptiveMaxTokens(6144, 30000)
  assert.ok(mid > 6144, `mid-size source scales above the floor (got ${mid})`)
  assert.equal(adaptiveMaxTokens(6144, 1000000), 16384, 'huge source caps at 16K')
  assert.equal(adaptiveMaxTokens(9000, 100), 9000, 'never below the configured current maxTokens')
})

test('breaker trips after N consecutive failures and throws instantly while open', () => {
  const g = createCompactionGuard({ tripThreshold: 2, cooldownMs: 5 * 60_000 })
  g.gate('s1', 10000, 6144)
  g.noteFailure('s1', new Error('TRANSPORT'))
  g.gate('s1', 10000, 6144)
  g.noteFailure('s1', new Error('TRANSPORT'))
  const s = g.stats('s1')
  assert.equal(s.phase, 'open')
  assert.equal(s.trips, 1)
  assert.equal(s.failures, 2)
  assert.ok(s.lastError, 'lastError recorded for observability')
  assert.throws(() => g.gate('s1', 10000, 6144), (err) => err.code === BREAKER_CODE)
  assert.equal(g.stats('s1').skips, 1)
  // skips cost nothing and never attempt the provider
  assert.equal(g.stats('s1').attempts, 2)
})

test('cooldown elapse allows exactly one half-open probe; failure re-opens with doubled cooldown', async () => {
  const g = createCompactionGuard({ tripThreshold: 1, cooldownMs: 15, maxCooldownMs: 60 })
  g.gate('s1', 100, 6144)
  g.noteFailure('s1')
  assert.equal(g.stats('s1').phase, 'open')
  await new Promise((r) => setTimeout(r, 25))
  const gate = g.gate('s1', 100, 6144) // half-open probe
  assert.ok(gate.maxTokens >= 6144)
  assert.equal(g.stats('s1').phase, 'half-open')
  g.noteFailure('s1') // probe fails -> re-open, cooldown doubled
  assert.equal(g.stats('s1').phase, 'open')
  assert.equal(g.stats('s1').cooldownMs, 30)
  assert.equal(g.stats('s1').trips, 2)
  await new Promise((r) => setTimeout(r, 35))
  g.gate('s1', 100, 6144)
  g.noteSuccess('s1') // probe succeeds -> close + reset
  assert.equal(g.stats('s1').phase, 'closed')
  assert.equal(g.stats('s1').cooldownMs, 15, 'success resets to the configured base cooldown')
  assert.equal(g.stats('s1').lastError, null)
})

test('success resets the failure counter; sessions are independent', () => {
  const g = createCompactionGuard({ tripThreshold: 2, cooldownMs: 60_000 })
  g.gate('s1', 100, 6144)
  g.noteFailure('s1')
  g.gate('s1', 100, 6144)
  g.noteSuccess('s1')
  g.gate('s1', 100, 6144)
  g.noteFailure('s1')
  assert.equal(g.stats('s1').phase, 'closed', 'one failure after a reset must not trip')
  const other = g.stats('s2')
  assert.equal(other.phase, 'closed')
  assert.equal(other.failures, 0)
})
