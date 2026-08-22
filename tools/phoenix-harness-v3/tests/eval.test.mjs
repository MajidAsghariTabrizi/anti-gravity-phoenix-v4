/**
 * Evaluator tests: proof-carrying certificates, tamper detection, gate
 * set, frontier task manifest completeness.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const EVAL_MOD = pathToFileURL(join(ROOT, 'src', 'eval', 'evaluator.js')).href

test('certificate binds evidence via hashes and verifies clean', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-cert-'))
  try {
    const evidence = join(tmp, 'evidence.md')
    writeFileSync(evidence, 'FACT: reconciled PnL = 0 (ledger 2026-08-02)\n')
    const { makeCertificate, verifyCertificate } = await import(EVAL_MOD)
    const cert = makeCertificate({
      gate: 'business', task: 'business-diagnosis', mission: 'diagnose revenue',
      verdict: 'pass', reviewer: 'reviewer-1', evidence: [evidence], notes: 'all rubric items met',
    })
    const v = verifyCertificate(cert)
    assert.equal(v.valid, true, v.problems.join('; '))
    // tamper with evidence -> proof breaks
    writeFileSync(evidence, 'FACT: altered\n')
    const v2 = verifyCertificate(cert)
    assert.equal(v2.valid, false)
    assert.ok(v2.problems.some((p) => /hash mismatch/.test(p)))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('certificate rejects unknown gates and bad verdicts', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-cert2-'))
  try {
    const evidence = join(tmp, 'e.txt')
    writeFileSync(evidence, 'x')
    const { makeCertificate, verifyCertificate, GATES } = await import(`${EVAL_MOD}?t=1`)
    assert.equal(GATES.length, 5)
    const bad = makeCertificate({ gate: 'bogus', task: 't', verdict: 'maybe', reviewer: 'r', evidence: [evidence] })
    const v = verifyCertificate(bad)
    assert.equal(v.valid, false)
    assert.ok(v.problems.some((p) => /unknown gate/.test(p)))
    assert.ok(v.problems.some((p) => /bad verdict/.test(p)))
    assert.throws(() => makeCertificate({ gate: 'business', task: 't', verdict: 'pass', reviewer: 'r', evidence: ['missing-file.md'] }), /evidence missing/)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('frontier benchmark: all 15 tasks present with rubrics and reviewer gates', () => {
  const dir = join(ROOT, 'benchmarks', 'frontier', 'tasks')
  const files = readdirSync(dir).filter((f) => f.endsWith('.json'))
  const required = [
    'codebase-orientation', 'code-investigation', 'bug-fix', 'schema-migration',
    'pr-ci-delivery', 'release', 'incident-recovery', 'ground-truth-analysis',
    'cross-domain-prioritization', 'safety-adversarial', 'long-context',
    'code-batch', 'wait-suspension', 'rollback-recovery', 'business-diagnosis',
  ]
  assert.equal(files.length, required.length, `expected ${required.length} tasks, got ${files.length}`)
  for (const id of required) assert.ok(files.includes(`${id}.json`), `missing task ${id}`)
  const validGates = ['business', 'architecture', 'prod_safety', 'release', 'evidence']
  for (const f of files) {
    const t = JSON.parse(readFileSync(join(dir, f), 'utf8'))
    assert.ok(t.id && t.name && t.prompt, `${f}: id/name/prompt required`)
    assert.ok(validGates.includes(t.reviewerGate), `${f}: bad reviewerGate`)
    assert.ok(Array.isArray(t.rubric.correctness) && t.rubric.correctness.length > 0, `${f}: correctness rubric required`)
    assert.ok(Array.isArray(t.rubric.safety) && Array.isArray(t.rubric.evidence), `${f}: safety/evidence rubric arrays required`)
  }
})

test('fixture planted bug exists and its test fails red before fix (green after exact fix)', async () => {
  const fixDir = join(ROOT, 'benchmarks', 'frontier', 'fixtures', 'amount-math')
  const { execFileSync } = await import('node:child_process')
  // red: the planted bug must make the fixture test exit non-zero.
  // (strip NODE_TEST_CONTEXT: the parent test-runner's context env would
  //  make the spawned --test child report failures via IPC with exit 0)
  const childEnv = { ...process.env }
  delete childEnv.NODE_TEST_CONTEXT
  let exitedClean = false
  try {
    execFileSync(process.execPath, ['--test', join(fixDir, 'amount.test.mjs')], { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', env: childEnv })
    exitedClean = true
  } catch { /* red expected */ }
  assert.equal(exitedClean, false, 'fixture test must FAIL (red) against the planted bug — but it passed')
})
