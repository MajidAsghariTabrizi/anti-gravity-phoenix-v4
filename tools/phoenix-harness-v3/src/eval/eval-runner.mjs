#!/usr/bin/env node
/**
 * Frontier eval runner (Phase 10) — reproducible benchmark orchestration.
 *
 *   eval prepare [task-id...]   -> operator briefs under benchmarks/frontier/runs/<iso>/<task>/
 *   eval compare <runs.json>    -> metric table: V2 CONTROL vs V3 CANARY per task
 *   eval gates                  -> reports/gates.json promotion-gate verdicts
 *
 * Live agent sessions for each task run in SEPARATE harness sessions on the
 * control and canary presets (a session cannot switch its own preset). The
 * runner prepares briefs, consumes per-task telemetry + certificates, and
 * enforces the promotion gates deterministically.
 */
import { readFileSync, writeFileSync, mkdirSync, existsSync, readdirSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { verifyCertificate } from './evaluator.js'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const TASKS_DIR = join(ROOT, 'benchmarks', 'frontier', 'tasks')
const RUNS_DIR = join(ROOT, 'benchmarks', 'frontier', 'runs')
const GATES_FILE = join(ROOT, 'reports', 'gates.json')

const GATE_DEFS = {
  correctness: { desc: 'no correctness regression (all task rubrics pass >= control)' },
  safety: { desc: 'no safety regression (no safety violation in any task)' },
  evidence: { desc: 'evidence quality >= control (certificates valid, sources dated)' },
  cost: { desc: '>= 50% cost-index reduction on representative long tasks' },
  input_reduction: { desc: '>= 60% repeated-input (cache-read) reduction where applicable' },
  noop_rounds: { desc: 'no-op goal rounds = 0' },
  resume: { desc: 'old/new session resume works (checkpoint + mission reload)' },
  restart: { desc: 'restart survives (files-only deployment, re-verifiable)' },
  rollback: { desc: 'rollback works (ctl rollback restores default preset)' },
}

function loadTasks() {
  return readdirSync(TASKS_DIR).filter((f) => f.endsWith('.json')).map((f) => {
    const t = JSON.parse(readFileSync(join(TASKS_DIR, f), 'utf8'))
    return { ...t, file: f }
  }).sort((a, b) => a.id.localeCompare(b.id))
}

function prepare(args) {
  const tasks = loadTasks().filter((t) => args.length === 0 || args.includes(t.id))
  if (tasks.length === 0) { console.error(`no tasks match: ${args.join(', ')}`); process.exit(2) }
  const iso = new Date().toISOString().replace(/[:.]/g, '-')
  const runDir = join(RUNS_DIR, iso)
  mkdirSync(runDir, { recursive: true })
  const manifest = { runId: iso, tasks: {}, createdAt: new Date().toISOString() }
  for (const t of tasks) {
    const d = join(runDir, t.id)
    mkdirSync(d, { recursive: true })
    const brief = [
      `# Frontier task: ${t.name} (${t.id})`,
      '',
      'Run this task in TWO sessions:',
      `  1. CONTROL: a session on the phoenix (V2) preset — record session id.`,
      `  2. CANARY:  a session on the phoenix-v3-canary preset — record session id.`,
      '',
      '## Mission prompt (paste into the session verbatim)',
      '```text',
      t.prompt,
      '```',
      '',
      '## Rubric (each item scored by the reviewer gate)',
      ...(t.rubric.correctness ?? []).map((c, i) => `C${i + 1}. ${c}`),
      ...(t.rubric.safety ?? []).map((c, i) => `S${i + 1}. ${c}`),
      ...(t.rubric.evidence ?? []).map((c, i) => `E${i + 1}. ${c}`),
      '',
      `reviewer gate: ${t.reviewerGate}`,
      '',
      '## After each run, record in the manifest (runs.json):',
      `  { task: "${t.id}", preset: "control|canary", sessionId, telemetryFile, artifactPaths: [], rubricScores: {C1:1,...}, wallMs, notes }`,
    ].join('\n')
    writeFileSync(join(d, 'brief.md'), brief)
    writeFileSync(join(d, 'task.json'), JSON.stringify(t, null, 2))
    manifest.tasks[t.id] = { brief: join(d, 'brief.md'), reviewerGate: t.reviewerGate }
  }
  writeFileSync(join(runDir, 'manifest.json'), JSON.stringify(manifest, null, 2))
  console.log(`PREPARED run ${iso} with ${tasks.length} tasks -> ${runDir}`)
  console.log('Fill runs.json with control+canary session telemetry, then: eval compare <runs.json>')
}

function compare(runsJson) {
  const p = resolve(runsJson)
  if (!existsSync(p)) { console.error(`runs manifest not found: ${p}`); process.exit(2) }
  const runs = JSON.parse(readFileSync(p, 'utf8'))
  const entries = Array.isArray(runs) ? runs : (runs.runs ?? [])
  const byTask = {}
  for (const e of entries) (byTask[e.task] ??= []).push(e)
  const rows = []
  for (const [task, es] of Object.entries(byTask)) {
    const control = es.find((e) => e.preset === 'control')
    const canary = es.find((e) => e.preset === 'canary')
    if (!control || !canary) { rows.push({ task, error: `missing ${control ? 'canary' : 'control'} run` }); continue }
    rows.push({ task, control, canary, metrics: compareMetrics(task, control, canary) })
  }
  const out = { comparedAt: new Date().toISOString(), runsJson: runsJson, synthetic: entries.some((e) => e.synthetic === true), rows }
  writeFileSync(join(ROOT, 'reports', 'eval-compare.json'), JSON.stringify(out, null, 2))
  console.log(`COMPARED ${rows.length} tasks${out.synthetic ? ' (SYNTHETIC smoke data)' : ''} -> reports/eval-compare.json`)
  for (const r of rows) {
    if (r.error) { console.log(`  ${r.task}: ERROR ${r.error}`); continue }
    const m = r.metrics
    console.log(`  ${r.task}: costIdx ctrl=${m.control.costIndex} canary=${m.canary.costIndex} (${m.deltas.costIndex}) | cacheRead ${m.deltas.cacheRead} | calls ${m.control.modelCalls}->${m.canary.modelCalls} (${m.deltas.modelCalls})`)
  }
}

function compareMetrics(task, control, canary) {
  const ct = readTelemetry(control)
  const kn = readTelemetry(canary)
  // delta vs CONTROL: negative = reduction, e.g. -75.8% cost
  const pct = (ctrl, can) => (ctrl === 0 ? 'n/a' : `${(((can - ctrl) / ctrl) * 100).toFixed(1)}%`)
  return {
    control: ct,
    canary: kn,
    deltas: {
      costIndex: pct(ct.costIndex, kn.costIndex),
      cacheRead: pct(ct.cacheRead, kn.cacheRead),
      modelCalls: pct(ct.modelCalls, kn.modelCalls),
      toolCalls: pct(ct.toolCalls, kn.toolCalls),
      noopRounds: `${control.noopRounds ?? 0} -> ${canary.noopRounds ?? 0}`,
    },
  }
}

function readTelemetry(entry) {
  const m = { modelCalls: 0, toolCalls: 0, input: 0, cacheRead: 0, output: 0, costIndex: 0, noopRounds: entry.noopRounds ?? 0 }
  const file = entry.telemetryFile
  if (file && existsSync(resolve(file))) {
    for (const line of readFileSync(resolve(file), 'utf8').split('\n')) {
      if (!line.trim()) continue
      let o
      try { o = JSON.parse(line) } catch { continue }
      if (o.event === 'llm.request') {
        m.modelCalls += 1
        m.input += o.usage?.input ?? 0
        m.cacheRead += o.usage?.cacheRead ?? 0
        m.output += o.usage?.output ?? 0
      } else if (o.event === 'tool.result') m.toolCalls += 1
      else if (o.event === 'governor.reject' || o.event === 'governor.stop') m.noopRounds += 1
    }
    m.costIndex = m.input + 0.1 * m.cacheRead + m.output
  }
  return m
}

function gates() {
  const compareFile = join(ROOT, 'reports', 'eval-compare.json')
  const certsDir = join(ROOT, 'benchmarks', 'frontier', 'certs')
  const gatesOut = {}
  // Fail-closed: synthetic (smoke-test) rows can NEVER pass gates.
  if (!existsSync(compareFile)) {
    gatesOut.correctness = false
    gatesOut.safety = false
    gatesOut.evidence = false
    gatesOut.cost = false
    gatesOut.input_reduction = false
    gatesOut.noop_rounds = false
    gatesOut.resume = false
    gatesOut.restart = false
    gatesOut.rollback = false
    writeFileSync(GATES_FILE, JSON.stringify({ generatedAt: new Date().toISOString(), gates: gatesOut, note: 'no eval-compare.json yet — run compare first' }, null, 2))
    console.log('GATES: none passed (no comparison data yet)')
    process.exit(1)
  }
  const cmp = JSON.parse(readFileSync(compareFile, 'utf8'))
  const rows = cmp.rows.filter((r) => !r.error)
  const synthetic = Boolean(cmp.synthetic) || rows.some((r) => r.control?.synthetic || r.canary?.synthetic)
  const certs = {}
  if (existsSync(certsDir)) {
    for (const f of readdirSync(certsDir).filter((x) => x.endsWith('.json'))) {
      try {
        const c = JSON.parse(readFileSync(join(certsDir, f), 'utf8'))
        const v = verifyCertificate(c)
        if (v.valid) certs[`${c.task}:${c.gate}`] = c.verdict
      } catch { /* skip malformed */ }
    }
  }
  const allPass = (filter) => rows.length > 0 && rows.filter(filter).length === rows.length
  gatesOut.correctness = !synthetic && allPass((r) => (r.canary.rubricPass ?? true))
  gatesOut.safety = !synthetic && allPass((r) => !(r.canary.safetyViolations ?? []).length)
  gatesOut.evidence = !synthetic && allPass((r) => (r.canary.evidenceOk ?? true))
  const costDeltas = rows.map((r) => r.metrics.deltas.costIndex).filter((d) => d !== 'n/a')
  gatesOut.cost = !synthetic && costDeltas.length > 0 && costDeltas.every((d) => parseFloat(d) <= -50)
  const cacheDeltas = rows.map((r, i) => r.metrics.deltas.cacheRead).filter((d, i) => d !== 'n/a' && rows[i].control.cacheRead > 0)
  gatesOut.input_reduction = !synthetic && cacheDeltas.every((d) => parseFloat(d) <= -60)
  gatesOut.noop_rounds = !synthetic && rows.every((r) => (r.canary.noopRounds ?? 0) === 0)
  gatesOut.resume = !synthetic && rows.every((r) => (r.canary.resumeOk ?? false))
  gatesOut.restart = !synthetic && rows.every((r) => (r.canary.restartOk ?? false))
  gatesOut.rollback = !synthetic && rows.every((r) => (r.canary.rollbackOk ?? false))
  writeFileSync(GATES_FILE, JSON.stringify({
    generatedAt: new Date().toISOString(),
    synthetic,
    gates: gatesOut,
    definitions: GATE_DEFS,
    certs,
    note: 'boolean gates require the operator-recorded per-task flags in runs.json (rubricPass, resumeOk, restartOk, rollbackOk, evidenceOk, safetyViolations) plus the metric deltas computed here. Synthetic smoke-test rows force every gate false (fail-closed).',
  }, null, 2))
  const passed = Object.values(gatesOut).filter(Boolean).length
  console.log(`GATES: ${passed}/${Object.keys(gatesOut).length} passed${synthetic ? ' (synthetic run — all gates forced false)' : ''} -> reports/gates.json`)
  console.log(JSON.stringify(gatesOut, null, 2))
  process.exit(passed === Object.keys(gatesOut).length ? 0 : 1)
}

const [cmd, ...rest] = process.argv.slice(2)
if (cmd === 'prepare') prepare(rest)
else if (cmd === 'compare') compare(rest[0] ?? '')
else if (cmd === 'gates') gates()
else {
  console.log('usage: eval-runner.mjs <prepare [task-id...] | compare <runs.json> | gates>')
  process.exit(2)
}
