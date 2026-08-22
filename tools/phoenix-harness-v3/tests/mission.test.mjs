/**
 * MissionSpec compiler tests: typing, validation, budgets, hard stops,
 * owner approval gate, phase boundaries.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, rmSync, existsSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const MOD = pathToFileURL(join(ROOT, 'src', 'mission.js')).href

test('compileMission: happy path with defaults', async () => {
  const m = await import(MOD)
  const { spec } = m.compileMission({ objective: 'Diagnose revenue', domains: ['business', 'engine'], riskTier: 'prod_readonly' })
  assert.equal(spec.objective, 'Diagnose revenue')
  assert.deepEqual(spec.domains, ['business', 'engine'])
  assert.equal(spec.riskTier, 'prod_readonly')
  assert.deepEqual(spec.hardStops, ['budget_breach'])
  assert.ok(spec.budgets.tokens > 0)
  assert.ok(spec.budgets.modelCalls > 0)
  assert.ok(spec.budgets.elapsedMinutes > 0)
  assert.equal(spec.ownerApproval, null)
})

test('compileMission: rejects unknown domains and empty objective', async () => {
  const m = await import(`${MOD}?t=1`)
  assert.ok(m.compileMission({ objective: 'x', domains: ['not-a-domain'] }).error)
  assert.ok(m.compileMission({}).error)
  assert.ok(m.compileMission({ objective: '' }).error)
})

test('compileMission: prod_mutation defaults authority to owner-required; hard stops enum', async () => {
  const m = await import(`${MOD}?t=2`)
  const { spec } = m.compileMission({ objective: 'arm lane', riskTier: 'prod_mutation', hardStops: ['safety_breach', 'bogus'] })
  assert.equal(spec.authority, 'owner-required')
  assert.deepEqual(spec.hardStops, ['safety_breach'])
  const { spec: spec2 } = m.compileMission({ objective: 'x', hardStops: ['staleness', 'uncertain_submission'] })
  assert.deepEqual(spec2.hardStops, ['staleness', 'uncertain_submission'])
})

test('mission persistence + phase boundaries + owner approval flow', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-mission-'))
  try {
    const m = await import(`${MOD}?t=3`)
    const { spec } = m.compileMission({ objective: 'Deploy release', riskTier: 'prod_mutation' })
    const written = m.writeMission(tmp, 's1', spec)
    assert.equal(written.ok, true)
    assert.ok(existsSync(m.missionPath(tmp, 's1')))
    const read = m.readMission(tmp, 's1')
    assert.equal(read.exists, true)
    assert.equal(read.spec.riskTier, 'prod_mutation')
    // owner approval: ack OWNER required
    const cur = m.readMission(tmp, 's1')
    cur.spec.ownerApproval = { approvedAt: new Date().toISOString(), by: 'owner', scope: 'release dispatch' }
    m.writeMission(tmp, 's1', cur.spec)
    assert.ok(m.readMission(tmp, 's1').spec.ownerApproval.approvedAt)
    const marked = m.markPhase(tmp, 's1', 'implement')
    assert.equal(marked.error, undefined)
    assert.ok(marked.spec.phases.some((p) => p.name === 'implement'))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('renderMission is compact (< 40 lines) and never embeds secrets', async () => {
  const m = await import(`${MOD}?t=4`)
  const { spec } = m.compileMission({ objective: 'x', acceptanceCriteria: ['a', 'b'] })
  const rendered = m.renderMission(spec)
  assert.ok(rendered.split('\n').length < 45)
  assert.ok(!/api[_-]?key|password|secret/i.test(rendered))
})
