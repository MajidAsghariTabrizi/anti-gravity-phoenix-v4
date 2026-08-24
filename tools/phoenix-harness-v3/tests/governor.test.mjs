/**
 * L-005/L-007/L-010 governor tests + deterministic wait core.
 * Verifies: usage accumulation, wait registry, no-op round rejection
 * ({kind:'reject'} = zero-cost blocked turn), hard-stop rejection on
 * goal-round steps, warning ratios, and waitForState resolve/deadline
 * semantics.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function setupGovernor(tmp) {
  const records = []
  const sink = { record: (sid, obj) => records.push({ sid, obj }) }
  return import(pathToFileURL(join(ROOT, 'src', 'governor.js')).href).then(({ createGovernor }) => {
    const gov = createGovernor(tmp, sink, {})
    return { gov, records }
  })
}

function writeMission(tmp, overrides = {}) {
  mkdirSync(join(tmp, '.phoenix-harness', 'checkpoints'), { recursive: true })
  const spec = {
    schema: 1, createdAt: new Date().toISOString(), objective: 't', domains: ['business'],
    riskTier: 'local_only', authority: 'agent-local', budgets: { tokens: 1000, modelCalls: 10, elapsedMinutes: 60 },
    hardStops: ['budget_breach'], acceptanceCriteria: [], evidenceRequirements: [],
    ownerApproval: null, phases: [], ...overrides,
  }
  writeFileSync(join(tmp, '.phoenix-harness', 'checkpoints', 'mission-s1.json'), JSON.stringify(spec, null, 2))
  return spec
}

test('governor accumulates billed-equivalent usage', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov-'))
  try {
    const { gov } = await setupGovernor(tmp)
    gov.noteUsage('s1', { input: 100, output: 50, cacheRead: 1000, cacheWrite: 0, reasoning: 0 })
    gov.noteUsage('s1', { input: 200, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 })
    const view = gov.budgetView('s1')
    assert.equal(view.measured.modelCalls, 2)
    assert.equal(view.measured.tokensBilledEq, 100 + 1000 * 0.1 + 50 + 200)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('governor rejects bookkeeping-only steps during an active wait (L-010)', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov2-'))
  try {
    const { gov, records } = await setupGovernor(tmp)
    writeMission(tmp)
    gov.registerWait('s1', 'ci:1', Date.now() + 60000, 'ci watch')
    gov.noteToolResult('s1', 'get_goal')
    gov.noteToolResult('s1', 'phoenix_checkpoint')
    const decision = gov.preStep('s1', { turn: 1, step: 1, messages: [] }, { kind: 'enter', messages: [] })
    assert.equal(decision.kind, 'reject', 'no-op round during wait must be rejected (zero-cost blocked turn)')
    assert.ok(records.some((r) => r.obj.event === 'governor.reject'))
    // a productive tool call resets the streak and unblocks
    gov.noteToolResult('s1', 'read')
    const decision2 = gov.preStep('s1', { turn: 1, step: 2, messages: [] }, { kind: 'enter', messages: [] })
    assert.equal(decision2.kind, 'enter')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('governor hard-stops goal rounds after budget breach (L-007 no blind continuation)', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov3-'))
  try {
    const { gov, records } = await setupGovernor(tmp)
    writeMission(tmp, { budgets: { tokens: 100, modelCalls: 10, elapsedMinutes: 60 } })
    gov.noteUsage('s1', { input: 900, output: 100, cacheRead: 0 })
    const goalMessages = [{ role: 'user', content: 'continue', source: { kind: 'goal', round: 1 } }]
    const decision = gov.preStep('s1', { turn: 2, step: 1, messages: goalMessages }, { kind: 'enter', messages: goalMessages })
    assert.equal(decision.kind, 'reject', 'budget-breached goal round must be stopped')
    assert.ok(records.some((r) => r.obj.event === 'governor.stop'))
    // non-goal steps (operator message) are never blocked — human authority wins
    const humanMessages = [{ role: 'user', content: 'proceed' }]
    const decision2 = gov.preStep('s1', { turn: 2, step: 2, messages: humanMessages }, { kind: 'enter', messages: humanMessages })
    assert.equal(decision2.kind, 'enter')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('governor warns near budgets without blocking', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov4-'))
  try {
    const { gov, records } = await setupGovernor(tmp)
    writeMission(tmp, { budgets: { tokens: 10000, modelCalls: 10, elapsedMinutes: 60 } })
    gov.noteUsage('s1', { input: 9000, output: 0, cacheRead: 0 })
    const decision = gov.preStep('s1', { turn: 1, step: 1, messages: [] }, { kind: 'enter', messages: [] })
    assert.equal(decision.kind, 'enter')
    const warns = records.filter((r) => r.obj.event === 'governor.warn')
    assert.ok(warns.length >= 1)
    assert.ok(warns[0].obj.ratio >= 0.8)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('waitForState resolves on change and fails closed at deadline', async () => {
  const { waitForState } = await import(pathToFileURL(join(ROOT, 'src', 'wait.js')).href)
  let calls = 0
  const ok = await waitForState(async () => { calls += 1; return calls >= 3 ? 'DONE' : null }, { intervalMs: 5, maxWaitMs: 5000 })
  assert.equal(ok.ok, true)
  assert.equal(ok.state, 'DONE')
  assert.equal(calls, 3)
  const deadline = await waitForState(async () => null, { intervalMs: 5, maxWaitMs: 40 })
  assert.equal(deadline.ok, false)
  assert.ok(/deadline/.test(deadline.error))
})

test('mission budgets are the delta from mission start (quickfix)', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov5-'))
  try {
    const { gov } = await setupGovernor(tmp)
    // pre-mission usage accumulates in the session totals
    gov.noteUsage('s1', { input: 5000, output: 100, cacheRead: 0 })
    gov.noteUsage('s1', { input: 2000, output: 0, cacheRead: 0 })
    const baseline = gov.usageSnapshot('s1')
    assert.equal(baseline.modelCalls, 2)
    assert.equal(baseline.tokens, 7100)
    // mission created NOW: baseline snapshotted at creation
    writeMission(tmp, {
      budgets: { tokens: 100000, modelCalls: 50, elapsedMinutes: 60 },
      usageBaseline: { ...baseline, startedAtMs: Date.now() },
    })
    gov.noteUsage('s1', { input: 900, output: 100, cacheRead: 0 })
    const view = gov.budgetView('s1')
    assert.equal(view.missionScoped, true)
    assert.equal(view.measured.modelCalls, 1, 'only post-mission calls count')
    assert.equal(view.measured.tokensBilledEq, 1000, 'only post-mission tokens count')
    assert.equal(view.ratios.modelCalls, 0.02)
    assert.ok(view.measured.elapsedMinutes >= 0 && view.measured.elapsedMinutes < 0.5)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('mission without a usage baseline keeps session totals (back-compat)', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-gov6-'))
  try {
    const { gov } = await setupGovernor(tmp)
    writeMission(tmp, { budgets: { tokens: 100000, modelCalls: 50, elapsedMinutes: 60 } })
    gov.noteUsage('s1', { input: 900, output: 100, cacheRead: 0 })
    const view = gov.budgetView('s1')
    assert.equal(view.missionScoped, false)
    assert.equal(view.measured.modelCalls, 1)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})
