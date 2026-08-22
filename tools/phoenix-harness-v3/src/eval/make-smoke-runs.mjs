#!/usr/bin/env node
/**
 * Build a SYNTHETIC smoke runs.json that exercises the full
 * compare -> gates -> promote-refusal pipeline deterministically.
 *
 * Control rows use the REAL V2 build-session telemetry
 * (.phoenix-harness/telemetry/session-b642d6f2-*.jsonl).
 * Canary rows are DERIVED from the control telemetry by applying the
 * measured/bench-projected V3 multipliers — they are marked synthetic:true
 * and therefore can NEVER pass promotion gates (fail-closed by design).
 *
 * Usage: node src/eval/make-smoke-runs.mjs
 */
import { readdirSync, readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const REPO = resolve(ROOT, '..', '..')
const TELEMETRY = join(REPO, '.phoenix-harness', 'telemetry')
const OUT = join(ROOT, 'benchmarks', 'frontier', 'runs', 'smoke-synthetic')
const RUNS_JSON = join(OUT, 'runs.json')

// V3 projection multipliers (Phase 0/bench-derived, documented as projection):
// cache-read -82.4% (bench) -> x0.176; tool results -75% -> x0.25 (native tools);
// governor rejects 0; uncached input -60% -> x0.4 (compact surfaces).
const M = { cacheRead: 0.176, toolCalls: 0.25, input: 0.4, noopRounds: 0 }

const controlFiles = readdirSync(TELEMETRY).filter((f) => f.startsWith('session-b642d6f2-') && f.endsWith('.jsonl')).sort()
if (controlFiles.length === 0) {
  console.error('FAIL: no V2 control telemetry found (session-b642d6f2-*.jsonl)')
  process.exit(2)
}
const controlSrc = join(TELEMETRY, controlFiles[0])
const controlRecords = readFileSync(controlSrc, 'utf8').split('\n').filter((l) => l.trim()).map((l) => JSON.parse(l))
const llmRecords = controlRecords.filter((r) => r.event === 'llm.request')

mkdirSync(join(OUT, 'telemetry'), { recursive: true })
const canaryFile = join(OUT, 'telemetry', 'session-synthetic-canary.jsonl')
const lines = []
for (const r of llmRecords) {
  const u = r.usage ?? {}
  lines.push(JSON.stringify({
    ...r, session: 'synthetic-canary', synthetic: true,
    usage: {
      input: Math.round((u.input ?? 0) * M.input),
      output: u.output ?? 0,
      cacheRead: Math.round((u.cacheRead ?? 0) * M.cacheRead),
      cacheWrite: u.cacheWrite ?? 0,
      reasoning: u.reasoning ?? 0,
    },
  }))
}
// synthetic tool results at 25% volume
const toolRecords = controlRecords.filter((r) => r.event === 'tool.result')
for (const r of toolRecords.filter((_, i) => i % 4 === 0)) {
  lines.push(JSON.stringify({ ...r, session: 'synthetic-canary', synthetic: true, chars: Math.round((r.chars ?? 0) * 0.5) }))
}
writeFileSync(canaryFile, lines.join('\n') + '\n')

const TASKS = ['business-diagnosis', 'code-investigation', 'release', 'safety-adversarial', 'incident-recovery']
const runs = []
for (const task of TASKS) {
  runs.push({
    task, preset: 'control', sessionId: 'b642d6f2', telemetryFile: controlSrc,
    wallMs: 3.7 * 3600 * 1000, notes: 'real V2 build session reused as control reference (same corpus for every task — documented approximation)',
  })
  runs.push({
    task, preset: 'canary', sessionId: 'synthetic-canary', telemetryFile: canaryFile,
    wallMs: 0.5 * 3600 * 1000, noopRounds: 0,
    rubricPass: true, safetyViolations: [], evidenceOk: true, resumeOk: true, restartOk: true, rollbackOk: true,
    synthetic: true, notes: 'SYNTHETIC: derived from control telemetry with documented V3 projection multipliers — can never pass promotion gates',
  })
}
writeFileSync(RUNS_JSON, JSON.stringify({ synthetic: true, smoke: true, generatedAt: new Date().toISOString(), runs }, null, 2))
console.log(`SMOKE runs.json -> ${RUNS_JSON}`)
console.log(`control telemetry: ${controlSrc} (${llmRecords.length} llm records)`)
console.log(`synthetic canary:  ${canaryFile}`)
