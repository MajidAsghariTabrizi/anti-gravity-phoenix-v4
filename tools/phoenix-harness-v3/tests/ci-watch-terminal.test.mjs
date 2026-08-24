/**
 * Focused reliability tests — TERMINAL-AWARE CI WATCH + ABORTABLE WAIT
 * (V3 reliability hardening, 2026-08-24). Scenarios:
 *   1  initial completed/success        -> immediate terminal return
 *   2  initial completed/failure        -> immediate terminal return
 *   3  initial completed/skipped        -> immediate terminal return
 *   4  queued -> in_progress            -> KEEP WAITING (no early wake)
 *   5  in_progress -> completed/success -> RETURN
 *   6  in_progress -> completed/failure -> RETURN (valid terminal, not tool failure)
 *   7  SHA mixed completed + pending    -> KEEP WAITING
 *   8  SHA all terminal                 -> RETURN (+ counts)
 *   9  AbortSignal during sleep         -> prompt exit, no extra check,
 *                                          governor wait ALWAYS cleared
 *   10 read error                       -> fail closed, wait cleared
 */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  isRealConclusion, isRunTerminal, parseRunState, parseCheckStates,
  summarizeChecks, allChecksTerminal, watchCi,
} from '../src/tools-native/ci-watch-core.js'
import { waitForState } from '../src/wait.js'
import { waitTool } from '../src/tools-native/ops.js'

/** Sequenced fake reader: pops states, repeats the last one forever. */
function seqReader(states) {
  let i = 0
  const reads = () => i
  const read = async () => {
    const value = states[Math.min(i, states.length - 1)]
    i += 1
    return value
  }
  return { read, reads }
}

test('scenarios 1-3: initial terminal runs return IMMEDIATELY (success/failure/skipped)', async () => {
  for (const [raw, conclusion] of [['completed|success', 'success'], ['completed|failure', 'failure'], ['completed|skipped', 'skipped']]) {
    const r = seqReader([raw])
    const out = await watchCi({ read: r.read, mode: 'run', intervalMs: 10, maxWaitMs: 60000 })
    assert.equal(out.ok, true, raw)
    assert.equal(out.immediate, true, raw)
    assert.equal(out.checks, 1, raw)
    assert.equal(out.waitedMs, 0, raw)
    const parsed = parseRunState(out.state)
    assert.deepEqual(parsed, { status: 'completed', conclusion })
    // re-entry stays cheap: a second call performs exactly one fresh read
    const again = await watchCi({ read: r.read, mode: 'run', intervalMs: 10, maxWaitMs: 60000 })
    assert.equal(again.immediate, true)
    assert.equal(r.reads(), 2, 'exactly one fresh GitHub read per re-entry')
  }
})

test('generic terminal predicate covers the full conclusion set without a whitelist', () => {
  for (const c of ['success', 'failure', 'cancelled', 'skipped', 'timed_out', 'action_required', 'neutral', 'stale']) {
    assert.ok(isRunTerminal('completed', c), c)
    assert.ok(isRealConclusion(c), c)
  }
  for (const bad of ['', 'pending', null, undefined, '-', 'none']) {
    assert.equal(isRunTerminal('completed', bad), false, String(bad))
  }
  assert.equal(isRunTerminal('in_progress', 'success'), false)
})

test('scenario 4: queued -> in_progress KEEPS WAITING; only a terminal outcome wakes it', async () => {
  const r = seqReader(['queued|-', 'in_progress|-', 'completed|success'])
  const out = await watchCi({ read: r.read, mode: 'run', intervalMs: 5, maxWaitMs: 5000 })
  assert.equal(out.ok, true)
  assert.equal(out.immediate, false)
  assert.equal(r.reads(), 3, 'non-terminal transitions did NOT resolve the watch')
  assert.equal(parseRunState(out.state).conclusion, 'success')

  const stuck = seqReader(['queued|-', 'in_progress|-'])
  const deadline = await watchCi({ read: stuck.read, mode: 'run', intervalMs: 5, maxWaitMs: 80 })
  assert.equal(deadline.ok, false)
  assert.match(deadline.error, /deadline/)
  assert.equal(stuck.reads() >= 2, true, 'kept polling while non-terminal')
})

test('scenarios 5-6: in_progress -> completed/{success,failure} RETURN as valid terminals', async () => {
  for (const final of ['completed|success', 'completed|failure']) {
    const r = seqReader(['in_progress|-', final])
    const out = await watchCi({ read: r.read, mode: 'run', intervalMs: 5, maxWaitMs: 5000 })
    assert.equal(out.ok, true)
    assert.equal(parseRunState(out.state).conclusion, final.split('|')[1])
  }
})

test('scenario 7: SHA with mixed completed+pending checks KEEPS WAITING until all terminal', async () => {
  const mixed = 'completed|success;in_progress|-;queued|-'
  assert.equal(allChecksTerminal(parseCheckStates(mixed)), false)
  const s = summarizeChecks(parseCheckStates(mixed))
  assert.deepEqual(s, { total: 3, success: 1, failure: 0, cancelled: 0, skipped: 0, pending: 2, other: 0 })

  const r = seqReader([mixed, mixed, 'completed|success;completed|failure'])
  const out = await watchCi({ read: r.read, mode: 'sha', intervalMs: 5, maxWaitMs: 5000 })
  assert.equal(out.ok, true)
  assert.equal(out.immediate, false)
  assert.equal(r.reads(), 3, 'partially-running SHA never reported final')
  const counts = summarizeChecks(parseCheckStates(out.state))
  assert.equal(counts.success, 1)
  assert.equal(counts.failure, 1)
  assert.equal(counts.pending, 0)
})

test('scenario 8: SHA all-terminal returns IMMEDIATELY with counts', async () => {
  const all = 'completed|success;completed|failure;completed|cancelled;completed|skipped'
  const r = seqReader([all])
  const out = await watchCi({ read: r.read, mode: 'sha', intervalMs: 10, maxWaitMs: 60000 })
  assert.equal(out.ok, true)
  assert.equal(out.immediate, true)
  assert.equal(out.checks, 1)
  const counts = summarizeChecks(parseCheckStates(out.state))
  assert.deepEqual(counts, { total: 4, success: 1, failure: 1, cancelled: 1, skipped: 1, pending: 0, other: 0 })
})

test('scenario 9: abort during sleep exits PROMPTLY, no extra check, timer cleared', async () => {
  const ac = new AbortController()
  let checks = 0
  const p = waitForState(async () => { checks += 1; return null }, { intervalMs: 250, maxWaitMs: 60000, signal: ac.signal })
  await new Promise((r) => setTimeout(r, 60))
  const t0 = Date.now()
  ac.abort()
  const res = await p
  const elapsed = Date.now() - t0
  assert.equal(res.aborted, true)
  assert.equal(res.ok, false)
  assert.ok(elapsed < 200, `prompt exit after abort (took ${elapsed}ms)`)
  const checksAtAbort = checks
  await new Promise((r) => setTimeout(r, 120))
  assert.equal(checks, checksAtAbort, 'NO extra check() after abort')
  // pre-aborted signal fails immediately without any check call
  const ac2 = new AbortController()
  ac2.abort()
  let called = false
  const res2 = await waitForState(async () => { called = true; return null }, { intervalMs: 10, maxWaitMs: 1000, signal: ac2.signal })
  assert.equal(res2.aborted, true)
  assert.equal(called, false)
})

/** Governor double recording register/clear. */
function fakeGovernor() {
  const g = { registered: [], cleared: [] }
  g.registerWait = (sid, id) => g.registered.push(id)
  g.clearWait = (sid, id) => g.cleared.push(id)
  return g
}

test('scenario 10: read error fails CLOSED and the registered wait is cleared', async () => {
  const out = await watchCi({ read: async () => { throw new Error('gh api failed: 404') }, mode: 'run', intervalMs: 5, maxWaitMs: 5000 })
  assert.equal(out.ok, false)
  assert.match(out.error, /cannot read CI state/)
  // mid-wait read error also fails closed
  let n = 0
  const mid = await watchCi({
    read: async () => { if (++n === 1) return 'in_progress|-'; throw new Error('network down') },
    mode: 'run', intervalMs: 5, maxWaitMs: 5000,
  })
  assert.equal(mid.ok, false)
  assert.match(mid.error, /wait check failed/)
})

test('governor wait bookkeeping cleared on EVERY exit path through the wait-tool seam', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'phx-wait-'))
  try {
    // (a) timeout path
    let gov = fakeGovernor()
    const tool = waitTool(gov, dir, () => 's1')
    const bad = await tool.execute({ target: 'file', path: 'never/exists.txt', timeoutMs: 60, intervalMs: 20 }, {})
    assert.match(bad, /WAIT FAILED/)
    assert.equal(gov.registered.length, 1)
    assert.equal(gov.cleared.length, 1)
    assert.equal(gov.cleared[0], gov.registered[0])

    // (b) abort path — aborted result still clears the registered wait
    gov = fakeGovernor()
    const ac = new AbortController()
    const execSig = { signal: ac.signal }
    setTimeout(() => ac.abort(), 30)
    const aborted = await waitTool(gov, dir, () => 's2').execute({ target: 'timeout', timeoutMs: 60000, intervalMs: 100 }, execSig)
    assert.match(aborted, /WAIT FAILED.*abort/i)
    assert.equal(gov.cleared.length, 1, 'wait cleared after abort')

    // (c) invalid target path
    gov = fakeGovernor()
    const inv = await waitTool(gov, dir, () => 's3').execute({ target: 'bogus' }, {})
    assert.match(inv, /error: target must be/)
    assert.equal(gov.cleared.length, 1)

    // (d) success path (file appears during wait)
    gov = fakeGovernor()
    const okPath = join(dir, 'appears.txt')
    setTimeout(() => writeFileSync(okPath, 'x'), 40)
    const okOut = await waitTool(gov, dir, () => 's4').execute({ target: 'file', path: 'appears.txt', timeoutMs: 5000, intervalMs: 15 }, {})
    assert.match(okOut, /WAIT OK/)
    assert.equal(gov.cleared.length, 1)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
