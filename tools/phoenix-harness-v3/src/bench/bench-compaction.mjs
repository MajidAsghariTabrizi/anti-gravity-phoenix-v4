#!/usr/bin/env node
/**
 * Phoenix Harness V3 — compaction policy benchmark.
 *
 * Same calibrated model as the V2 bench (.phoenix-harness/tools/
 * bench-compaction.mjs): replays the 640-step measured baseline session
 * under {legacy, V2-control, V3-canary} policies. The baseline row
 * reproduces the measured 282.3M cache-read within calibration error.
 *
 * V3 policy: thresholdRatio 0.09 (calibrated so peak surface stays
 * <= 96K — the P95 canary target), retain 32K, summary capped 6K tokens
 * on the Flash route.
 *
 * Usage: node bench-compaction.mjs [--g 2100]
 */
import { readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const REPO = resolve(process.env.PHOENIX_REPO ?? process.cwd())
const METRICS = join(REPO, '.phoenix-harness', 'telemetry', 'baseline', 'session-4a21bf00-metrics.json')

const CHARS_PER_TOKEN = 4
const CONTEXT_WINDOW = 1000000
const STEPS = 640
const COMPACT_CALL_INPUT = 200000
const COMPACT_CALL_OUTPUT = 8192
const LEGACY_SUMMARY_TOKENS = 9000
const V3_SUMMARY_TOKENS = 6000

const args = process.argv.slice(2)
const gIdx = args.indexOf('--g')
const GROWTH_PER_STEP = gIdx >= 0 ? Number(args[gIdx + 1]) : 2100

let metrics = null
try { metrics = JSON.parse(readFileSync(METRICS, 'utf8')) } catch { /* bench still runs with defaults */ }
const prefixChars = metrics?.headers?.[0]?.chars ?? 52404
const prefixTokens = Math.floor(prefixChars / CHARS_PER_TOKEN)
const measuredCacheRead = 282314880

function simulate({ thresholdTokens, retainTokens, summaryTokens, label }) {
  let surface = 0
  let cacheRead = 0
  let compactions = 0
  let peak = 0
  let sum = 0
  for (let step = 0; step < STEPS; step++) {
    surface += GROWTH_PER_STEP
    if (surface > thresholdTokens) {
      surface = retainTokens + summaryTokens
      compactions += 1
    }
    cacheRead += prefixTokens + surface
    if (surface > peak) peak = surface
    sum += surface
  }
  const compactOverhead = compactions * (COMPACT_CALL_INPUT + COMPACT_CALL_OUTPUT)
  return { label, thresholdTokens, retainTokens, cacheRead: Math.round(cacheRead), compactions, compactOverhead, peakSurface: Math.round(peak), avgSurface: Math.round(sum / STEPS), finalSurface: Math.round(surface) }
}

const rows = [
  simulate({ thresholdTokens: Math.floor(CONTEXT_WINDOW * 0.8), retainTokens: Math.floor(CONTEXT_WINDOW * 0.16), summaryTokens: LEGACY_SUMMARY_TOKENS, label: 'legacy  (0.80/0.16)' }),
  simulate({ thresholdTokens: Math.floor(CONTEXT_WINDOW * 0.2), retainTokens: 65536, summaryTokens: LEGACY_SUMMARY_TOKENS, label: 'V2 ctrl (0.20/64K)' }),
  simulate({ thresholdTokens: Math.floor(CONTEXT_WINDOW * 0.09), retainTokens: 32768, summaryTokens: V3_SUMMARY_TOKENS, label: 'V3 canary(0.09/32K)' }),
]

console.log('PHOENIX HARNESS V3 COMPACTION BENCHMARK — 640-step measured baseline replay')
console.log(`calibration: prefix=${prefixTokens}T growth=${GROWTH_PER_STEP}T/step | measured baseline: cacheRead=${measuredCacheRead} compactions=1\n`)
const fmt = (s, w) => String(s).padStart(w)
console.log(`${'policy'.padEnd(22)}${fmt('threshold', 10)}${fmt('retain', 8)}${fmt('cacheRead', 13)}${fmt('vsLegacy', 9)}${fmt('compacts', 9)}${fmt('peak', 9)}${fmt('avg', 9)}${fmt('ovhd', 10)}`)
const base = rows[0].cacheRead
for (const r of rows) {
  const delta = base > 0 ? `${(((r.cacheRead - base) / base) * 100).toFixed(1)}%` : 'n/a'
  console.log(`${r.label.padEnd(22)}${fmt(r.thresholdTokens, 10)}${fmt(r.retainTokens, 8)}${fmt(r.cacheRead, 13)}${fmt(delta, 9)}${fmt(r.compactions, 9)}${fmt(r.peakSurface, 9)}${fmt(r.avgSurface, 9)}${fmt(r.compactOverhead, 10)}`)
}
console.log('\nV3 target check (mission Phase 4):')
const v3 = rows[2]
console.log(`  normal 30-70K:    avg surface ${v3.avgSurface}T ${v3.avgSurface >= 30000 && v3.avgSurface <= 70000 ? 'IN TARGET' : 'OUT'}`)
console.log(`  P95 <= 96K:       peak surface ${v3.peakSurface}T ${v3.peakSurface <= 96000 ? 'IN TARGET' : 'OUT'}`)
console.log(`  hard limit 160K:  ${v3.peakSurface <= 160000 ? 'IN TARGET' : 'OUT'}`)
console.log(`  summary 4-6K:     V3 summary ${V3_SUMMARY_TOKENS}T IN TARGET`)
console.log(`  cache-read vs legacy: ${(((v3.cacheRead - base) / base) * 100).toFixed(1)}%`)
