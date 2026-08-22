#!/usr/bin/env node
/**
 * Phoenix Harness V3 — live automated A/B eval orchestrator (Phase 2/4).
 *
 *   node live-runner.mjs prepare [--campaign runs/<id>]
 *   node live-runner.mjs run --campaign runs/<id> [--stage debug|final]
 *        [--arms control,candidate] [--tasks a,b] [--runs N] [--budget-min M]
 *   node live-runner.mjs review --campaign runs/<id>
 *   node live-runner.mjs gates --campaign runs/<id> [--final]
 *   node live-runner.mjs cleanup
 *
 * Contract (see benchmarks/frontier/RUN_PLAN.md):
 *  - one child PROCESS per (task, arm, run): boots the pinned checkout,
 *    mounts the arm preset under a temp worktree cwd, runs the task, writes
 *    a fail-closed run record; the parent enforces the budget.
 *  - control arm = untouched V2 `phoenix` preset (native tool mode);
 *    candidate arm = fresh V3 `phoenix-v3-canary` install (DSH_TOOLS_MODE=code);
 *    judge arm = bare base agent (no preset).
 *  - checkers are deterministic; anything they cannot decide goes to the
 *    anonymized A/B judge. gates computes the mission gate matrix and writes
 *    REAL_GATES.json (reports/gates.json only with --final, all-pass).
 *
 * PHOENIX_EVAL_FAKE_CHILD=1 substitutes a stub child that writes a synthetic
 * run record — for evaluator-mechanics tests ONLY (never for real gates).
 */
import { readFileSync, writeFileSync, readdirSync, existsSync, mkdirSync, rmSync, copyFileSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import {
  REPO_ROOT, checkoutPath, verifyCheckout, realDshHome, presetIdFor, toolsModeFor,
  armHomeFor, prepareArmHome, shaOf, createWorktree, removeWorktree,
  plantTaskFixtures, copyLocalContext, taskPromptText, collectSessionEvidence, run,
} from './campaign.mjs'
import { checkerFor } from './checkers.mjs'
import { judgePrompt, parseVerdict, mapVerdict } from './judge.mjs'
import { gateReport, writeRealGates } from './gates.js'

const SRC = dirname(fileURLToPath(import.meta.url))
const TASKS_DIR = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'benchmarks', 'frontier', 'tasks')
const RUNS_ROOT = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'benchmarks', 'frontier', 'runs')
const RUN_ONE = join(SRC, 'run-one.mjs')
const argv = process.argv.slice(2)
const cmd = argv[0] ?? 'help'
const flag = (name, fallback) => {
  const i = argv.indexOf(`--${name}`)
  return i === -1 ? fallback : argv[i + 1]
}
const hasFlag = (name) => argv.includes(`--${name}`)

function loadTasks() {
  const out = {}
  for (const f of readdirSync(TASKS_DIR).filter((f) => f.endsWith('.json'))) {
    try { out[f.replace(/\.json$/, '')] = JSON.parse(readFileSync(join(TASKS_DIR, f), 'utf8')) } catch { /* skip broken */ }
  }
  return out
}

function campaignRoot(name) {
  return name ? (resolve(name) === name ? name : join(RUNS_ROOT, name)) : RUNS_ROOT
}

function writeJson(p, obj) {
  mkdirSync(dirname(p), { recursive: true })
  writeFileSync(p, JSON.stringify(obj, null, 2))
}

function readJson(p, fallback) {
  try { return JSON.parse(readFileSync(p, 'utf8')) } catch { return fallback }
}

function spawnChild(args, opts) {
  return new Promise((resolveFn) => {
    const child = spawn(process.execPath, args, {
      cwd: opts.cwd ?? REPO_ROOT,
      env: { ...process.env, ...(opts.env ?? {}) },
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    })
    let out = ''
    let err = ''
    let settled = false
    const timer = setTimeout(() => {
      if (settled) return
      settled = true
      try { child.kill() } catch { /* already gone */ }
      resolveFn({ ok: false, killed: true, code: 'BUDGET', stdout: out, stderr: err + '\n[budget-killed]' })
    }, opts.budgetMs ?? 45 * 60000)
    child.stdout.on('data', (d) => { out += String(d) })
    child.stderr.on('data', (d) => { err += String(d) })
    child.on('error', (e) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      resolveFn({ ok: false, killed: false, code: 'SPAWN', stdout: out, stderr: `${err}\n${e?.message ?? e}` })
    })
    child.on('close', (code) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      resolveFn({ ok: code === 0, killed: false, code, stdout: out, stderr: err })
    })
  })
}

async function prepare(args) {
  const checkout = checkoutPath()
  const pin = verifyCheckout(checkout)
  if (!pin.ok) return { ok: false, error: pin.error }
  const id = new Date().toISOString().replace(/[:.]/g, '-')
  const root = join(campaignRoot(flag('campaign', `eval-${id}`)))
  mkdirSync(root, { recursive: true })
  const shaRes = await shaOf('HEAD')
  const sha = shaRes.ok ? shaRes.stdout.trim() : null
  const realHome = realDshHome()
  const arms = {}
  for (const arm of ['control', 'candidate', 'judge']) {
    const home = armHomeFor(root, arm)
    const prep = await prepareArmHome(home, arm, { realHome })
    if (!prep.ok) return { ok: false, error: prep.error }
    arms[arm] = { home, preset: presetIdFor(arm), toolsMode: toolsModeFor(arm) ?? 'native' }
  }
  const manifest = {
    schema: 'phoenix.eval.campaign.v1',
    id,
    createdAt: new Date().toISOString(),
    checkout, pinnedVersion: pin.version,
    repo: { root: REPO_ROOT, sha, branch: (await shaOf('--abbrev-ref', undefined))?.stdout?.trim?.() ?? null },
    arms,
    model: (() => {
      try {
        const s = readFileSync(join(realHome, 'settings.yaml'), 'utf8')
        const m = /model:\s*(\S+)/.exec(s)
        const p = /provider:\s*(\S+)/.exec(s)
        return { provider: p?.[1] ?? null, model: m?.[1] ?? null }
      } catch { return { provider: null, model: null } }
    })(),
    tasks: Object.keys(loadTasks()),
  }
  writeJson(join(root, 'manifest.json'), manifest)
  return { ok: true, root, manifest }
}

async function runCampaign(args) {
  const root = join(campaignRoot(flag('campaign')))
  const manifest = readJson(join(root, 'manifest.json'), null)
  if (!manifest) return { ok: false, error: `campaign manifest missing: ${root}` }
  const pin = verifyCheckout(manifest.checkout)
  if (!pin.ok) return { ok: false, error: `checkout pin failed: ${pin.error}` }
  const stage = flag('stage', 'debug')
  const budgetMin = Number(flag('budget-min', stage === 'final' ? 45 : 15))
  const runsN = Number(flag('runs', stage === 'final' ? 3 : 1))
  const tasks = loadTasks()
  const taskIds = (flag('tasks') ? String(flag('tasks')).split(',') : Object.keys(tasks)).filter((t) => tasks[t])
  const arms = (flag('arms') ? String(flag('arms')).split(',') : ['control', 'candidate']).filter((a) => manifest.arms[a])
  const sha = manifest.repo.sha
  const results = []
  for (const arm of arms) {
    for (const taskId of taskIds) {
      for (let r = 0; r < runsN; r++) {
        const armHome = manifest.arms[arm].home
        const runDir = join(root, 'runs', arm, taskId, `r${r}`)
        mkdirSync(runDir, { recursive: true })
        const wt = join(root, 'tmp', 'worktrees', arm, taskId, `r${r}`)
        const entry = { arm, taskId, run: r, startedAt: new Date().toISOString(), sha, runDir, ok: false, error: null }
        console.error(`[eval] ${arm} ${taskId} r${r} — worktree at ${sha.slice(0, 8)}`)
        const created = await createWorktree(sha, wt)
        if (!created.ok) {
          entry.error = created.error
          results.push(entry)
          continue
        }
        copyLocalContext(wt)
        const planted = await plantTaskFixtures(wt, taskId)
        if (!planted.ok) {
          entry.error = `fixture plant failed: ${planted.error}`
          await removeWorktree(wt)
          results.push(entry)
          continue
        }
        const sessionIdPlaceholder = `eval-${arm}-${taskId}-r${r}`
        const promptFile = join(runDir, 'prompt.md')
        writeFileSync(promptFile, taskPromptText(tasks[taskId], { sessionId: sessionIdPlaceholder, arm, taskId }))
        const outFile = join(runDir, 'run.json')
        const preset = manifest.arms[arm].preset
        let childResult
        if (process.env.PHOENIX_EVAL_FAKE_CHILD === '1') {
          // Evaluator-mechanics stub — synthetic run records only (tests).
          childResult = { ok: true, killed: false, code: 0, stdout: '', stderr: 'synthetic' }
          writeJson(outFile, {
            schema: 'phoenix.eval.run.v1', synthetic: true, presetId: preset, taskId, run: r,
            startedAt: entry.startedAt, finishedAt: new Date().toISOString(), wallMs: 1000,
            sessionId: `synthetic-${arm}-${taskId}-r${r}`, ok: true, exitCode: 0,
            finalText: 'SYNTHETIC FAKE RUN — mechanics test only.\n' + (tasks[taskId].prompt ?? '').slice(0, 200),
            finalTextLen: 120, reason: { kind: 'completed' },
            digest: { types: { 'assistant/message': 1, 'turn/end': 1 }, tools: { phoenix_context: 1 } },
            model: manifest.model,
          })
        } else {
          childResult = await spawnChild([
            RUN_ONE,
            '--preset', preset || 'none',
            '--task', taskId,
            '--run', String(r),
            '--worktree', wt,
            '--prompt-file', promptFile,
            '--dsh-home', armHome,
            '--out', outFile,
            '--budget-ms', String(budgetMin * 60000),
          ], {
            cwd: wt,
            budgetMs: budgetMin * 60000,
            env: {
              DSH_HOME: armHome,
              PHOENIX_DSH_CHECKOUT: manifest.checkout,
              ...(manifest.arms[arm].toolsMode === 'code' ? { DSH_TOOLS_MODE: 'code' } : {}),
            },
          })
        }
        const record = readJson(outFile, null)
        entry.ok = record?.ok === true
        entry.exitCode = record?.exitCode ?? childResult.code
        entry.wallMs = record?.wallMs ?? null
        entry.sessionId = record?.sessionId ?? null
        entry.synthetic = record?.synthetic === true
        entry.finalTextLen = record?.finalTextLen ?? 0
        entry.reason = record?.reason ?? null
        entry.killed = childResult.killed === true
        entry.stderrTail = String(childResult.stderr ?? '').slice(-2000)
        if (entry.sessionId) {
          const evidence = collectSessionEvidence(
            [join(wt, '.phoenix-harness'), join(armHome, '.phoenix-harness')],
            entry.sessionId, runDir)
          entry.evidenceFiles = evidence
        }
        // Capture fixture state before teardown (bug-fix checker needs it).
        const fixState = join(runDir, 'evidence', 'fixture-state')
        mkdirSync(fixState, { recursive: true })
        const fixDir = join(wt, 'tools', 'phoenix-harness-v3', 'benchmarks', 'frontier', 'fixtures', 'amount-math')
        if (existsSync(fixDir)) {
          for (const f of readdirSync(fixDir).filter((x) => !x.endsWith('.test.mjs'))) {
            copyFileSync(join(fixDir, f), join(fixState, f))
          }
        }
        entry.finishedAt = new Date().toISOString()
        writeJson(join(runDir, 'entry.json'), entry)
        await removeWorktree(wt)
        results.push(entry)
      }
    }
  }
  writeJson(join(root, 'results.json'), results)
  const okCount = results.filter((x) => x.ok).length
  console.error(`[eval] run done: ${okCount}/${results.length} completed`)
  return { ok: true, results }
}

async function reviewCampaign(args) {
  const root = join(campaignRoot(flag('campaign')))
  const manifest = readJson(join(root, 'manifest.json'), null)
  if (!manifest) return { ok: false, error: 'campaign manifest missing' }
  const tasks = loadTasks()
  const reviews = {}
  for (const taskId of Object.keys(tasks)) {
    const task = tasks[taskId]
    const pair = {}
    for (const arm of ['control', 'candidate']) {
      const runDir = join(root, 'runs', arm, taskId, 'r0')
      const record = readJson(join(runDir, 'run.json'), null)
      pair[arm] = record
    }
    const review = { taskId, task: task.name, checker: null, judge: null, verdict: 'inconclusive', notes: [] }
    const checker = checkerFor(taskId)
    if (checker) {
      const candidateRecord = pair.candidate ?? {}
      const checkerArgs = {
        worktreeDir: join(root, 'runs', 'candidate', taskId, 'r0', 'evidence', 'fixture-state'),
        finalText: candidateRecord.finalText ?? '',
        evidenceDir: join(root, 'runs', 'candidate', taskId, 'r0', 'evidence'),
        runDir: join(root, 'runs', 'candidate', taskId, 'r0'),
      }
      const res = await checker(checkerArgs)
      review.checker = res
      if (res.verdict !== 'inconclusive') {
        review.verdict = res.verdict === 'pass' ? 'candidate_pass' : 'candidate_fail'
        reviews[taskId] = review
        continue
      }
      review.notes.push(`checker inconclusive: ${res.checks.map((c) => c.id).join(',')}`)
    }
    // Anonymized judge: shuffle arm order per task.
    const order = Math.random() < 0.5 ? { x: 'control', y: 'candidate' } : { x: 'candidate', y: 'control' }
    const aText = pair[order.x]?.finalText ?? '(no output — run failed)'
    const bText = pair[order.y]?.finalText ?? '(no output — run failed)'
    const prompt = judgePrompt(task, aText, bText)
    const judgeDir = join(root, 'reviews', taskId)
    mkdirSync(judgeDir, { recursive: true })
    writeJson(join(judgeDir, 'judge-prompt.json'), { taskId, order, promptLen: prompt.length })
    writeFileSync(join(judgeDir, 'judge-prompt.txt'), prompt)
    review.judge = { order, raw: null, mapped: null, status: 'pending' }
    if (process.env.PHOENIX_EVAL_FAKE_CHILD !== '1') {
      const judgeHome = manifest.arms.judge.home
      const outFile = join(judgeDir, 'judge-run.json')
      const child = await spawnChild([
        RUN_ONE,
        '--preset', 'none',
        '--task', `judge-${taskId}`,
        '--run', '0',
        '--worktree', REPO_ROOT,
        '--prompt-file', join(judgeDir, 'judge-prompt.txt'),
        '--dsh-home', judgeHome,
        '--out', outFile,
        '--budget-ms', String(15 * 60000),
      ], {
        cwd: REPO_ROOT,
        budgetMs: 15 * 60000,
        env: { DSH_HOME: judgeHome, PHOENIX_DSH_CHECKOUT: manifest.checkout },
      })
      const judgeRecord = readJson(outFile, null)
      if (judgeRecord?.finalText) {
        const parsed = parseVerdict(judgeRecord.finalText)
        review.judge.raw = parsed
        review.judge.mapped = parsed ? mapVerdict(parsed, order) : null
        review.judge.status = parsed ? 'parsed' : 'unparseable'
        review.judge.unparseableText = parsed ? undefined : judgeRecord.finalText.slice(0, 800)
        if (parsed) {
          review.verdict = parsed.verdict === 'both_fail' ? 'both_fail'
            : parsed.verdict === 'tie' ? 'tie'
            : parsed.verdict === 'x_wins' ? (order.x === 'candidate' ? 'candidate_wins' : 'control_wins')
            : (order.y === 'candidate' ? 'candidate_wins' : 'control_wins')
        }
      } else {
        review.judge.status = 'judge-failed'
        review.notes.push(`judge run failed: ${judgeRecord?.reason ?? 'no record'}`)
      }
    }
    reviews[taskId] = review
  }
  writeJson(join(root, 'reviews.json'), reviews)
  return { ok: true, reviews }
}

async function gatesCmd(args) {
  const root = join(campaignRoot(flag('campaign')))
  const report = gateReport(root)
  writeRealGates(root, report)
  if (hasFlag('--final') && report.allPass) {
    const ctlGates = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'reports', 'gates.json')
    writeJson(ctlGates, report.gates)
    console.error(`[eval] FINAL: all gates pass — promotion gate file written: ${ctlGates}`)
  } else if (hasFlag('--final')) {
    console.error('[eval] FINAL requested but gates fail — reports/gates.json NOT written (fail-closed)')
  }
  console.log(JSON.stringify(report, null, 2))
  return { ok: true, report }
}

async function cleanup(args) {
  const { cleanupEvalPrs } = await import('./campaign.mjs')
  const prs = await cleanupEvalPrs()
  const tmp = join(campaignRoot(flag('campaign')), 'tmp')
  if (existsSync(tmp)) rmSync(tmp, { recursive: true, force: true })
  console.error(`[eval] cleanup: closed=${prs.closed} branches-deleted=${prs.deleted} tmp removed=${!existsSync(tmp)}`)
  return { ok: true, prs }
}

async function main() {
  try {
    if (cmd === 'prepare') return await prepare(argv)
    if (cmd === 'run') return await runCampaign(argv)
    if (cmd === 'review') return await reviewCampaign(argv)
    if (cmd === 'gates') return await gatesCmd(argv)
    if (cmd === 'cleanup') return await cleanup(argv)
    console.error('usage: live-runner.mjs prepare|run|review|gates|cleanup [--campaign runs/<id>] [flags]')
    return { ok: false, error: 'unknown command' }
  } catch (err) {
    console.error(`[eval] fatal: ${err?.message ?? err}`)
    return { ok: false, error: String(err?.message ?? err) }
  }
}

const res = await main()
process.exit(res?.ok === false ? 1 : 0)
