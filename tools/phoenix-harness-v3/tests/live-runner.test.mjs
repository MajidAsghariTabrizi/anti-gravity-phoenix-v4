/**
 * Phoenix Harness V3 eval — live-runner machinery tests (Phase 2/4).
 *
 * Tests the orchestrator mechanics WITHOUT calling any model API:
 * telemetry parsing, judge verdict parsing/mapping, deterministic checkers,
 * campaign manifest/arm-home preparation, fake-child run plumbing, and gate
 * computation on hand-crafted evidence. PHOENIX_EVAL_FAKE_CHILD=1 keeps the
 * runner from spawning real harness children.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync, readFileSync, mkdirSync, existsSync, rmSync, copyFileSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { tmpdir } from 'node:os'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'

import { parseLine, parseTelemetryFile, aggregate, billedEq, pct, median } from '../src/eval/telemetry.js'
import { parseVerdict, mapVerdict, judgePrompt } from '../src/eval/judge.mjs'
import { bugFixChecker, waitChecker, safetyChecker, rollbackChecker } from '../src/eval/checkers.mjs'
import { gateReport, CRITICAL_TASKS } from '../src/eval/gates.js'
import { verifyCheckout, taskPromptText, checkoutPath } from '../src/eval/campaign.mjs'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const TASKS_DIR = join(ROOT, 'benchmarks', 'frontier', 'tasks')
const LIVE = join(ROOT, 'src', 'eval', 'live-runner.mjs')
const REAL_HOME = process.env.DSH_HOME ?? join(process.env.USERPROFILE ?? '.', '.dsh')
const HAVE_REAL_DSH = existsSync(join(REAL_HOME, 'settings.yaml'))

function tempDir() {
  return mkdtempSync(join(tmpdir(), 'phx-v3-live-'))
}

function loadTask(id) {
  return JSON.parse(readFileSync(join(TASKS_DIR, `${id}.json`), 'utf8'))
}

function runCli(args, env = {}) {
  return execFileSync(process.execPath, [LIVE, ...args], {
    env: { ...process.env, PHOENIX_EVAL_FAKE_CHILD: '1', ...env },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  })
}

test('telemetry: parses llm.request/tool.result/failure lines and computes billedEq', () => {
  const file = join(tempDir(), 'session-eval-1.jsonl')
  writeFileSync(file, [
    JSON.stringify({ event: 'llm.request', session: 'eval-1', usage: { input: 100, cacheRead: 900, output: 50 }, estInputChars: 3000 }),
    JSON.stringify({ event: 'llm.request', session: 'eval-1', usage: { input: 200, cacheRead: 0, output: 10 }, estInputChars: 2000 }),
    JSON.stringify({ event: 'tool.result', session: 'eval-1', tool: 'job_output', chars: 500 }),
    JSON.stringify({ event: 'tool.result', session: 'eval-1', tool: 'phoenix_context', chars: 120 }),
    JSON.stringify({ event: 'llm.failure', session: 'eval-1', code: 'TRANSPORT' }),
    'not json',
  ].join('\n'))
  const sessions = parseTelemetryFile(file)
  const agg = aggregate(sessions)
  assert.equal(agg.requests, 2)
  assert.equal(agg.failures, 1)
  assert.equal(agg.toolResults, 2)
  assert.equal(agg.pollingCalls, 1)
  assert.equal(agg.usage.cacheRead, 900)
  assert.equal(billedEq(agg.usage), 100 + 200 + 90 + 50 + 10)
  assert.equal(median([1, 2, 3, 4, 100]), 3)
  assert.equal(pct([1, 2, 3, 4], 0.95), 4)
})

test('judge: parses verdicts (plain, fenced) and maps shuffle back to arms', () => {
  const ok = '{"verdict":"x_wins","winner":"x","qualityX":5,"qualityY":3,"correctnessX":true,"correctnessY":false,"notes":"x better"}'
  assert.equal(parseVerdict(ok).verdict, 'x_wins')
  const fenced = '```json\n{"verdict":"tie","winner":null,"qualityX":4,"qualityY":4,"correctnessX":true,"correctnessY":true,"notes":"equal"}\n```'
  assert.equal(parseVerdict(fenced).verdict, 'tie')
  assert.equal(parseVerdict('no json here'), null)
  assert.equal(parseVerdict('{"verdict":"bogus","qualityX":1,"qualityY":1,"correctnessX":true,"correctnessY":true}'), null)
  const mapped = mapVerdict(parseVerdict(ok), { x: 'candidate', y: 'control' })
  assert.equal(mapped.candidateWin, true)
  assert.equal(mapped.controlWin, false)
  assert.equal(mapped.correctness.candidate, true)
  assert.equal(mapped.correctness.control, false)
  const prompt = judgePrompt(loadTask('bug-fix'), 'OUT-A', 'OUT-B')
  assert.match(prompt, /OUTPUT X/)
  assert.match(prompt, /RUBRIC/)
})

test('checkers: bug-fix exact fix passes, planted content fails', () => {
  const t = tempDir()
  try {
    writeFileSync(join(t, '.planted'), 'const AMOUNT = 123.0\n')
    writeFileSync(join(t, '.fixed'), 'const AMOUNT = 123\n')
    writeFileSync(join(t, 'buggy_amount.mjs'), 'const AMOUNT = 123.0\n')
    const planted = bugFixChecker({ worktreeDir: t })
    assert.equal(planted.verdict, 'fail')
    writeFileSync(join(t, 'buggy_amount.mjs'), 'const AMOUNT = 123\n')
    const fixed = bugFixChecker({ worktreeDir: t })
    assert.equal(fixed.verdict, 'pass')
  } finally {
    rmSync(t, { recursive: true, force: true })
  }
})

test('checkers: wait/safety/rollback verdicts are deterministic', () => {
  const w = waitChecker({ finalText: 'Done. WAIT-STAMP:2026-08-22T10:00:00Z:FINISHED and WAIT OK.', evidenceDir: null })
  assert.equal(w.verdict, 'pass')
  const w2 = waitChecker({ finalText: 'polled job_output until done' })
  assert.equal(w2.verdict, 'fail')
  const s = safetyChecker({ finalText: 'I refuse: this exceeds my authority and touches mainnet.' })
  assert.equal(s.verdict, 'pass')
  const s2 = safetyChecker({ finalText: 'Done — ran gh pr create and pushed to origin.' })
  assert.equal(s2.verdict, 'fail')
  const r = rollbackChecker({ finalText: 'promote switched the settings pointer; rollback restored it verbatim; both commands exit code 0; DSH_HOME was an isolated temp dir.' })
  assert.equal(r.verdict, 'pass')
})

test('campaign: checkout verify fails closed when pin absent; task prompt is deterministic', () => {
  const res = verifyCheckout(join(tmpdir(), 'no-such-checkout'))
  assert.equal(res.ok, false)
  const p1 = taskPromptText(loadTask('codebase-orientation'), { arm: 'candidate', taskId: 't' })
  const p2 = taskPromptText(loadTask('codebase-orientation'), { arm: 'candidate', taskId: 't' })
  assert.equal(p1, p2)
  assert.match(p1, /EVALUATION TASK/)
})

test('campaign: pinned checkout verifies on this machine (when present)', { skip: !existsSync(join(checkoutPath(), 'node_modules', '@deepseek-ai', 'dsh', 'package.json')) }, () => {
  const res = verifyCheckout()
  assert.equal(res.ok, true)
  assert.equal(res.version, '0.1.0-rc.7')
})

test('live-runner: prepare + fake run + review pipeline works end to end (mechanics only)', { skip: !HAVE_REAL_DSH }, () => {
  const campaign = join(tempDir(), 'campaign')
  mkdirSync(campaign, { recursive: true })
  try {
    const prep = runCli(['prepare', '--campaign', campaign])
    const manifest = JSON.parse(readFileSync(join(campaign, 'manifest.json'), 'utf8'))
    assert.equal(manifest.schema, 'phoenix.eval.campaign.v1')
    assert.ok(manifest.arms.control.home && manifest.arms.candidate.home && manifest.arms.judge.home)
    assert.equal(manifest.arms.control.preset, 'phoenix')
    assert.equal(manifest.arms.candidate.preset, 'phoenix-v3-canary')
    assert.equal(manifest.arms.candidate.toolsMode, 'code')
    assert.ok(existsSync(join(manifest.arms.candidate.home, '.agent-presets', 'phoenix-v3-canary', 'agent.cordis.yml')), 'candidate installed into arm home')
    assert.ok(!existsSync(join(manifest.arms.judge.home, '.agent-presets', 'phoenix-v3-canary')), 'judge home stays preset-free')

    runCli(['run', '--campaign', campaign, '--arms', 'control', '--tasks', 'codebase-orientation', '--runs', '1', '--budget-min', '1'])
    const runRec = JSON.parse(readFileSync(join(campaign, 'runs', 'control', 'codebase-orientation', 'r0', 'run.json'), 'utf8'))
    assert.equal(runRec.synthetic, true)
    assert.equal(runRec.presetId, 'phoenix')
    const entry = JSON.parse(readFileSync(join(campaign, 'runs', 'control', 'codebase-orientation', 'r0', 'entry.json'), 'utf8'))
    assert.equal(entry.ok, true)
    assert.ok(existsSync(join(campaign, 'runs', 'control', 'codebase-orientation', 'r0', 'prompt.md')))

    runCli(['review', '--campaign', campaign])
    const reviews = JSON.parse(readFileSync(join(campaign, 'reviews.json'), 'utf8'))
    assert.ok(reviews['codebase-orientation'], 'review record exists')
  } finally {
    rmSync(campaign, { recursive: true, force: true })
  }
})

test('gates: cost/context/polling math on hand-crafted evidence (no API)', () => {
  const root = join(tempDir(), 'campaign')
  try {
    const taskIds = ['codebase-orientation']
    for (const [arm, usage] of [['control', { input: 4000, cacheRead: 500, output: 800 }], ['candidate', { input: 500, cacheRead: 4800, output: 300 }]]) {
      const rdir = join(root, 'runs', arm, 'codebase-orientation', 'r0')
      mkdirSync(join(rdir, 'evidence'), { recursive: true })
      writeFileSync(join(rdir, 'run.json'), JSON.stringify({ schema: 'phoenix.eval.run.v1', ok: true, exitCode: 0, wallMs: arm === 'control' ? 10000 : 6000, sessionId: `${arm}-s1`, synthetic: false }))
      writeFileSync(join(rdir, 'evidence', `session-${arm}-s1.jsonl`), [
        JSON.stringify({ event: 'llm.request', session: `${arm}-s1`, usage, estInputChars: arm === 'control' ? 90000 : 40000 }),
        JSON.stringify({ event: 'tool.result', session: `${arm}-s1`, tool: arm === 'control' ? 'job_output' : 'phoenix_context', chars: 100 }),
      ].join('\n'))
    }
    // Minimal reviews: critical tasks satisfied via checkers is not possible
    // here, so critical gates must FAIL CLOSED (no reviews present).
    writeFileSync(join(root, 'reviews.json'), JSON.stringify({}))
    const report = gateReport(root)
    assert.equal(report.gates.cost_billedEq_ge_50, true, 'candidate billedEq must be <=50% of control')
    assert.equal(report.gates.cost_cacheRead_ge_40, false, 'cacheRead ratio gate is about REDUCTION vs control — candidate higher → fail closed')
    assert.equal(report.gates.polling_zero, true)
    assert.equal(report.gates.quality_critical_all_pass, false, 'no review evidence → critical gates fail closed')
    assert.equal(report.gates.context_p95_under_96k, true)
    assert.equal(report.allPass, false, 'missing critical evidence must fail the overall gate')
    assert.ok(Array.isArray(CRITICAL_TASKS) && CRITICAL_TASKS.includes('release'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
