/**
 * Tests for the operations native tools (Phase 3B/3G/3H):
 * phoenix_current_truth, phoenix_changed_surface, phoenix_test_matrix,
 * phoenix_ci_snapshot, phoenix_wait.
 *
 * All tests are DSH_HOME-independent and worktree-safe: the current-truth
 * tool writes only under a temp root; the wait tool uses temp files;
 * ci_snapshot fails closed when gh data is unavailable.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { dirname, resolve } from 'node:path'
import { currentTruthTool, changedSurfaceTool, testMatrixTool, ciSnapshotTool, waitTool } from '../src/tools-native/ops.js'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function tempDir() {
  return mkdtempSync(join(tmpdir(), 'phx-v3-ops-'))
}

test('phoenix_current_truth: get before update fails, update/get roundtrip works, caps enforced', async () => {
  const t = tempDir()
  try {
    const tool = currentTruthTool(t)
    const missing = await tool.execute({ action: 'get' })
    assert.match(missing, /no current-truth capsule/)

    const up = await tool.execute({
      action: 'update', mission: 'test mission', nextAction: 'run tests',
      decisions: ['d1', 'd2'], blockers: ['b1'],
    })
    assert.match(up, /current truth updated/)
    const got = await tool.execute({ action: 'get' })
    assert.match(got, /test mission/)
    assert.match(got, /b1/)
    assert.match(got, /decisions=2/)

    // incremental merge: dedup + append
    await tool.execute({ action: 'update', decisions: ['d2', 'd3'] })
    const got2 = await tool.execute({ action: 'get' })
    assert.match(got2, /decisions=3/)

    // hard cap: a mega blocker list is capped/refused but never crashes
    const big = Array.from({ length: 60 }, (_, i) => `blocker-${i}-${'x'.repeat(300)}`)
    const res = await tool.execute({ action: 'update', blockers: big })
    assert.match(res, /updated|refused/)
    const file = join(t, '.phoenix-harness', 'CURRENT_TRUTH.json')
    assert.ok(existsSync(file))
    const parsed = JSON.parse(readFileSync(file, 'utf8'))
    assert.equal(parsed.schema, 'phoenix.current-truth.v1')
    assert.ok(JSON.stringify(parsed).length <= 12000, 'hard cap respected')
  } finally {
    rmSync(t, { recursive: true, force: true })
  }
})

test('phoenix_changed_surface returns git delta without throwing', async () => {
  const tool = changedSurfaceTool(ROOT)
  const out = await tool.execute({})
  assert.match(out, /CHANGED SURFACE/)
  assert.match(out, /dirty=(true|false)/)
})

test('phoenix_test_matrix lists suites and run commands', async () => {
  const tool = testMatrixTool(ROOT)
  const out = await tool.execute({})
  assert.match(out, /TEST MATRIX/)
  assert.match(out, /node --test tools\/phoenix-harness-v3\/tests\/\*\.test\.mjs/)
})

test('phoenix_ci_snapshot fails closed without a gh result (no crash)', async () => {
  const tool = ciSnapshotTool(ROOT)
  const out = await tool.execute({ limit: 2 })
  assert.match(out, /CI SNAPSHOT|error:/)
})

test('phoenix_wait: file target wakes on appearance; content target wakes on substring; deadline fails closed', async () => {
  const t = tempDir()
  const gov = { registered: [], cleared: [], registerWait: (sid, id, deadline, reason) => gov.registered.push({ sid, id, reason }), clearWait: (sid, id) => gov.cleared.push(id) }
  const tool = waitTool(gov, t, () => 'sid-1')
  const marker = 'marker.txt'
  try {
    // file target: write the file after 300ms
    const writing = setTimeout(() => writeFileSync(join(t, marker), 'READY:42'), 300)
    const res = await tool.execute({ target: 'file', path: marker, timeoutMs: 5000, intervalMs: 100 }, {})
    clearTimeout(writing)
    assert.match(res, /WAIT OK/)
    assert.match(res, /0 model polling rounds/)
    assert.equal(gov.registered.length, 1)
    assert.equal(gov.cleared.length, 1)

    // content target: file exists but content arrives later
    writeFileSync(join(t, 'c.txt'), 'partial')
    const late = setTimeout(() => writeFileSync(join(t, 'c.txt'), 'partial then COMPLETE'), 300)
    const res2 = await tool.execute({ target: 'content', path: 'c.txt', contains: 'COMPLETE', timeoutMs: 5000, intervalMs: 100 }, {})
    clearTimeout(late)
    assert.match(res2, /WAIT OK/)

    // deadline fail-closed
    const res3 = await tool.execute({ target: 'file', path: 'never.txt', timeoutMs: 400, intervalMs: 100 }, {})
    assert.match(res3, /WAIT FAILED/)
    assert.match(res3, /fail closed/)

    // unknown target refused
    const res4 = await tool.execute({ target: 'bogus', path: 'x' }, {})
    assert.match(res4, /target must be file \| content \| timeout/)
  } finally {
    rmSync(t, { recursive: true, force: true })
  }
})

test('phoenix_wait: timeout target returns after a bounded sleep', async () => {
  const tool = waitTool({ registerWait() {}, clearWait() {} }, tempDir(), () => 'sid')
  const started = Date.now()
  const res = await tool.execute({ target: 'timeout', timeoutMs: 500, intervalMs: 200 }, {})
  const elapsed = Date.now() - started
  assert.match(res, /WAIT OK/)
  assert.ok(elapsed >= 400 && elapsed < 5000, `bounded sleep: ${elapsed}ms`)
})
