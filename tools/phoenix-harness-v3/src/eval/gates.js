/**
 * Phoenix Harness V3 eval — promotion gate computation (Phase 4).
 *
 * Consumes a campaign root (manifest.json, results.json, reviews.json, run
 * records + telemetry evidence) and computes the mission gate matrix:
 * COST / CONTEXT / POLLING / BOOKKEEPING / RELIABILITY / LATENCY / QUALITY /
 * COMPLETION. All gates fail closed on missing data.
 */
import { readFileSync, readdirSync, existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { REPO_ROOT } from './campaign.mjs'
import { parseTelemetryDir, aggregate, pct, median, billedEq } from './telemetry.js'

export const CRITICAL_TASKS = ['bug-fix', 'pr-ci-delivery', 'release', 'safety-adversarial']

function readJson(p, fallback) {
  try { return JSON.parse(readFileSync(p, 'utf8')) } catch { return fallback }
}

function loadTaskIds() {
  const dir = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'benchmarks', 'frontier', 'tasks')
  return readdirSync(dir).filter((f) => f.endsWith('.json')).map((f) => f.replace(/\.json$/, ''))
}

function perArm(root, arm, taskIds) {
  const stats = {
    requests: 0, billedEq: 0, cacheRead: 0, output: 0, failures: 0,
    toolResults: 0, polling: 0, bookkeeping: 0, estChars: [], wallMs: [],
    toolCallRuns: 0,
  }
  const byTask = {}
  for (const taskId of taskIds) {
    const rdir = join(root, 'runs', arm, taskId)
    if (!existsSync(rdir)) continue
    const t = { requests: 0, billedEq: 0, cacheRead: 0, wallMs: [], completed: 0, runs: 0, sessions: [] }
    for (const r of readdirSync(rdir).filter((x) => x.startsWith('r'))) {
      const record = readJson(join(rdir, r, 'run.json'), null)
      if (!record || record.synthetic) continue
      t.runs += 1
      if (record.ok) t.completed += 1
      t.wallMs.push(record.wallMs ?? 0)
      stats.wallMs.push(record.wallMs ?? 0)
      t.sessions.push(record.sessionId ?? null)
      const sessions = parseTelemetryDir(join(rdir, r, 'evidence'))
      const agg = aggregate(sessions)
      t.requests += agg.requests
      t.billedEq += billedEq(agg.usage)
      t.cacheRead += agg.usage.cacheRead
      stats.requests += agg.requests
      stats.billedEq += billedEq(agg.usage)
      stats.cacheRead += agg.usage.cacheRead
      stats.failures += agg.failures
      stats.toolResults += agg.toolResults
      stats.polling += agg.pollingCalls
      stats.bookkeeping += agg.bookkeepingCalls
      stats.estChars.push(...agg.estInputChars)
      if (agg.toolResults > 0) stats.toolCallRuns += 1
    }
    if (t.runs > 0) byTask[taskId] = t
  }
  return { stats, byTask }
}

const ratio = (a, b) => (b === 0 ? (a === 0 ? 1 : Infinity) : a / b)

export function gateReport(root) {
  const reviews = readJson(join(root, 'reviews.json'), {})
  const taskIds = loadTaskIds()
  const control = perArm(root, 'control', taskIds)
  const candidate = perArm(root, 'candidate', taskIds)
  const gates = {}
  const evidence = {}

  gates.cost_billedEq_ge_50 = ratio(candidate.stats.billedEq, control.stats.billedEq) <= 0.5
  gates.cost_cacheRead_ge_40 = ratio(candidate.stats.cacheRead, control.stats.cacheRead) <= 0.6
  gates.cost_requests_ge_40 = ratio(candidate.stats.requests, control.stats.requests) <= 0.6
  gates.context_p95_under_96k = (pct(candidate.stats.estChars, 0.95) ?? Infinity) <= 96000
  gates.context_no_overflow = candidate.stats.estChars.length > 0 && Math.max(...candidate.stats.estChars) <= 160000
  gates.polling_zero = candidate.stats.polling === 0
  gates.bookkeeping_le_20 = candidate.stats.bookkeeping === 0 || ratio(candidate.stats.bookkeeping, control.stats.bookkeeping) <= 0.2
  gates.reliability_failures_le_control = candidate.stats.failures <= control.stats.failures
  const cMed = median(candidate.stats.wallMs)
  const vMed = median(control.stats.wallMs)
  gates.latency_median_le_control = cMed !== null && vMed !== null && cMed <= vMed

  const criticalPass = CRITICAL_TASKS.every((t) => {
    const rev = reviews[t]
    if (!rev) return false
    if (rev.checker) return rev.checker.verdict === 'pass'
    return rev.judge?.mapped?.correctness?.candidate === true
  })
  gates.quality_critical_all_pass = criticalPass
  const judged = Object.values(reviews).filter((r) => r.judge?.mapped)
  gates.quality_no_task_lower = judged.length > 0 && judged.every((r) => r.judge.mapped.quality.candidate >= r.judge.mapped.quality.control)
  gates.quality_overall_not_lower = judged.filter((r) => r.judge.mapped.correctness.candidate).length >= judged.filter((r) => r.judge.mapped.correctness.control).length
  gates.completion_candidate = Object.keys(candidate.byTask).length === taskIds.length && Object.values(candidate.byTask).every((t) => t.runs > 0)

  evidence.cost = {
    control: { billedEq: control.stats.billedEq, cacheRead: control.stats.cacheRead, requests: control.stats.requests },
    candidate: { billedEq: candidate.stats.billedEq, cacheRead: candidate.stats.cacheRead, requests: candidate.stats.requests },
    ratios: {
      billedEq: ratio(candidate.stats.billedEq, control.stats.billedEq),
      cacheRead: ratio(candidate.stats.cacheRead, control.stats.cacheRead),
      requests: ratio(candidate.stats.requests, control.stats.requests),
    },
  }
  evidence.context = {
    p95estCharsCandidate: pct(candidate.stats.estChars, 0.95),
    maxEstCharsCandidate: candidate.stats.estChars.length ? Math.max(...candidate.stats.estChars) : null,
  }
  evidence.polling = { candidatePollingCalls: candidate.stats.polling, candidateBookkeepingCalls: candidate.stats.bookkeeping, controlBookkeepingCalls: control.stats.bookkeeping }
  evidence.reliability = { controlFailures: control.stats.failures, candidateFailures: candidate.stats.failures }
  evidence.latency = { controlMedianWallMs: vMed, candidateMedianWallMs: cMed }
  evidence.quality = { judgedTasks: judged.length, critical: CRITICAL_TASKS.map((t) => ({ task: t, verdict: reviews[t]?.verdict ?? 'missing' })) }
  evidence.completion = {
    candidateTasksWithRuns: Object.keys(candidate.byTask).length,
    expectedTasks: taskIds.length,
    candidateCompleted: Object.values(candidate.byTask).reduce((a, t) => a + t.completed, 0),
    candidateRuns: Object.values(candidate.byTask).reduce((a, t) => a + t.runs, 0),
  }
  const allPass = Object.values(gates).every(Boolean)
  return { gates, evidence, allPass, reviewedAt: new Date().toISOString() }
}

export function writeRealGates(root, report) {
  const p = join(root, 'REAL_GATES.json')
  mkdirSync(root, { recursive: true })
  writeFileSync(p, JSON.stringify(report, null, 2))
  return p
}
