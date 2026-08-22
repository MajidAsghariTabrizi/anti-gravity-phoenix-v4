#!/usr/bin/env node
/**
 * Phoenix Harness V3 — Phase 0 telemetry forensics analyzer.
 *
 * Input: Phoenix V2 live telemetry corpus (.phoenix-harness/telemetry/session-*.jsonl).
 * Output: per-session and corpus aggregates quantifying model calls, tool calls,
 * tools-per-request granularity, cache/fresh input, output/reasoning, retries,
 * tool-result volume, repeated operations, bookkeeping/no-op goal rounds, and
 * token-based cost proxies.
 *
 * Usage:
 *   node analyze-telemetry.mjs <telemetryDir> [--out out.json] [--text]
 *
 * Never prints message content — aggregates only.
 */
import { readFileSync, writeFileSync, readdirSync, mkdirSync } from 'node:fs'
import { join, resolve, basename, dirname } from 'node:path'

const CACHE_HIT_DISCOUNT = 0.1 // documented assumption: cache-hit billed at 0.1x input price
const BOOKKEEPING_TOOLS = new Set([
  'get_goal', 'update_goal', 'phoenix_checkpoint', 'job_list', 'list_agents', 'create_goal',
])
const POLLING_TOOLS = new Set(['job_output', 'job_list', 'list_agents'])

const args = process.argv.slice(2)
const dir = resolve(args.find((a) => !a.startsWith('--')))
const outIdx = args.indexOf('--out')

/** Normalize tool arguments for repeat fingerprinting. */
function normalizeArgs(raw) {
  if (raw === undefined || raw === null) return ''
  if (typeof raw !== 'string') return JSON.stringify(raw ?? null) ?? 'null'
  try {
    const obj = JSON.parse(raw)
    return JSON.stringify(sortDeep(obj))
  } catch {
    return raw.length > 500 ? raw.slice(0, 500) : raw
  }
}
function sortDeep(v) {
  if (Array.isArray(v)) return v.map(sortDeep)
  if (v !== null && typeof v === 'object') {
    const out = {}
    for (const k of Object.keys(v).sort()) out[k] = sortDeep(v[k])
    return out
  }
  return v
}

function newSessionStat(id) {
  return {
    id,
    lines: 0,
    requests: 0,
    failures: 0,
    failureCodes: {},
    toolResults: 0,
    toolResultChars: 0,
    toolMaxChars: 0,
    tools: new Map(), // name -> {calls, chars, maxChars}
    fingerprints: new Map(), // `${tool}|${fp}` -> {count, preview}
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 },
    estInputChars: { sum: 0, max: 0, n: 0 },
    reqInputTokens: [], // input + cacheRead per request
    tiers: {},
    finishes: {},
    turns: new Set(),
    bookkeepingByRequest: 0, // requests whose only tool calls are bookkeeping
    turnsWithTools: new Map(), // turn -> Set<tool>
  }
}

function parseLine(line, sessions) {
  let obj
  try { obj = JSON.parse(line) } catch { return }
  if (!obj || !obj.event) return
  const sid = obj.session ?? '(unknown)'
  let s = sessions.get(sid)
  if (!s) { s = newSessionStat(sid); sessions.set(sid, s) }
  s.lines += 1
  if (obj.turn !== undefined) s.turns.add(obj.turn)

  if (obj.event === 'llm.request') {
    s.requests += 1
    const u = obj.usage ?? {}
    s.usage.input += u.input ?? 0
    s.usage.output += u.output ?? 0
    s.usage.cacheRead += u.cacheRead ?? 0
    s.usage.cacheWrite += u.cacheWrite ?? 0
    s.usage.reasoning += u.reasoning ?? 0
    const reqTokens = (u.input ?? 0) + (u.cacheRead ?? 0)
    s.reqInputTokens.push(reqTokens)
    if (obj.estInputChars !== undefined && obj.estInputChars !== null) {
      s.estInputChars.sum += obj.estInputChars
      s.estInputChars.n += 1
      s.estInputChars.max = Math.max(s.estInputChars.max, obj.estInputChars)
    }
    if (obj.tier) s.tiers[obj.tier] = (s.tiers[obj.tier] ?? 0) + 1
    if (obj.failure) {
      s.failures += 1
      const code = typeof obj.failure === 'string' ? obj.failure : (obj.failure.code ?? obj.failure.type ?? '(object)')
      s.failureCodes[code] = (s.failureCodes[code] ?? 0) + 1
    }
    if (obj.finish) s.finishes[obj.finish] = (s.finishes[obj.finish] ?? 0) + 1
  } else if (obj.event === 'llm.failure') {
    s.failures += 1
    const code = obj.code ?? obj.error ?? '(unknown)'
    s.failureCodes[code] = (s.failureCodes[code] ?? 0) + 1
  } else if (obj.event === 'tool.result') {
    s.toolResults += 1
    const chars = obj.chars ?? 0
    s.toolResultChars += chars
    s.toolMaxChars = Math.max(s.toolMaxChars, chars)
    const name = obj.tool ?? '(unknown)'
    const t = s.tools.get(name) ?? { calls: 0, chars: 0, maxChars: 0 }
    t.calls += 1
    t.chars += chars
    t.maxChars = Math.max(t.maxChars, chars)
    s.tools.set(name, t)
    if (obj.turn !== undefined) {
      let set = s.turnsWithTools.get(obj.turn)
      if (!set) { set = new Set(); s.turnsWithTools.set(obj.turn, set) }
      set.add(name)
    }
    if (obj.args !== undefined) {
      const fp = normalizeArgs(obj.args)
      if (fp && fp !== '{}' && fp !== '') {
        const key = `${name}|${fp}`
        const e = s.fingerprints.get(key) ?? { count: 0, preview: '' }
        e.count += 1
        const previewRaw = typeof obj.args === 'string' ? obj.args : JSON.stringify(obj.args)
        e.preview = previewRaw.length > 120 ? previewRaw.slice(0, 120) + '…' : previewRaw
        s.fingerprints.set(key, e)
      }
    }
  }
}

function quantile(sorted, q) {
  if (sorted.length === 0) return 0
  const idx = Math.min(sorted.length - 1, Math.floor(q * sorted.length))
  return sorted[idx]
}

function summarize(s) {
  const req = s.requests
  const toolsPerRequest = req ? (s.toolResults / req).toFixed(2) : '0'
  const inputSorted = [...s.reqInputTokens].sort((a, b) => a - b)
  const avgReq = req ? Math.round(inputSorted.reduce((a, b) => a + b, 0) / req) : 0
  const billedInputEq = Math.round(s.usage.input + CACHE_HIT_DISCOUNT * s.usage.cacheRead)
  const costIndex = billedInputEq + s.usage.output // output at input price (documented assumption)
  const repeated = [...s.fingerprints.values()].filter((e) => e.count > 1)
  const repeatedCalls = repeated.reduce((a, e) => a + e.count, 0)
  const pollingCalls = [...s.tools.entries()].reduce((a, [n, t]) => a + (POLLING_TOOLS.has(n) ? t.calls : 0), 0)
  const bookkeepingTurns = [...s.turnsWithTools.entries()].filter(([, tools]) => {
    if (tools.size === 0) return false
    return [...tools].every((n) => BOOKKEEPING_TOOLS.has(n))
  }).length
  return {
    id: s.id,
    lines: s.lines,
    turns: s.turns.size,
    requests: req,
    failures: s.failures,
    failureCodes: s.failureCodes,
    toolsPerRequest: Number(toolsPerRequest),
    toolResults: s.toolResults,
    toolResultChars: s.toolResultChars,
    toolMaxChars: s.toolMaxChars,
    usage: s.usage,
    estInputCharsAvg: s.estInputChars.n ? Math.round(s.estInputChars.sum / s.estInputChars.n) : 0,
    estInputCharsMax: s.estInputChars.max,
    reqInputAvg: avgReq,
    reqInputMax: inputSorted.length ? inputSorted[inputSorted.length - 1] : 0,
    reqInputP95: quantile(inputSorted, 0.95),
    reqInputMedian: quantile(inputSorted, 0.5),
    tiers: s.tiers,
    finishes: s.finishes,
    billedInputEq,
    costIndex,
    repeatedOps: repeated.length,
    repeatedCalls,
    topRepeats: repeated.sort((a, b) => b.count - a.count).slice(0, 8).map((e) => ({ count: e.count, preview: e.preview })),
    pollingCalls,
    bookkeepingOnlyTurns: bookkeepingTurns,
    topTools: [...s.tools.entries()].sort((a, b) => b[1].calls - a[1].calls).slice(0, 12).map(([n, t]) => ({ tool: n, calls: t.calls, chars: t.chars })),
    biggestResults: [...s.tools.entries()].sort((a, b) => b[1].maxChars - a[1].maxChars).slice(0, 5).map(([n, t]) => ({ tool: n, maxChars: t.maxChars })),
  }
}

const files = readdirSync(dir).filter((f) => /^session-.*\.jsonl$/.test(f))
const sessions = new Map()
for (const f of files) {
  const text = readFileSync(join(dir, f), 'utf8')
  for (const line of text.split('\n')) {
    if (line.trim()) parseLine(line, sessions)
  }
}

const rows = [...sessions.values()].map(summarize).sort((a, b) => b.requests - a.requests)
const corpus = {
  files: files,
  sessions: rows.map((r) => r.id),
  requests: rows.reduce((a, r) => a + r.requests, 0),
  toolResults: rows.reduce((a, r) => a + r.toolResults, 0),
  toolResultChars: rows.reduce((a, r) => a + r.toolResultChars, 0),
  failures: rows.reduce((a, r) => a + r.failures, 0),
  usage: {
    input: rows.reduce((a, r) => a + r.usage.input, 0),
    output: rows.reduce((a, r) => a + r.usage.output, 0),
    cacheRead: rows.reduce((a, r) => a + r.usage.cacheRead, 0),
    cacheWrite: rows.reduce((a, r) => a + r.usage.cacheWrite, 0),
    reasoning: rows.reduce((a, r) => a + r.usage.reasoning, 0),
  },
  billedInputEq: rows.reduce((a, r) => a + r.billedInputEq, 0),
  costIndex: rows.reduce((a, r) => a + r.costIndex, 0),
  repeatedOps: rows.reduce((a, r) => a + r.repeatedOps, 0),
  repeatedCalls: rows.reduce((a, r) => a + r.repeatedCalls, 0),
  bookkeepingOnlyTurns: rows.reduce((a, r) => a + r.bookkeepingOnlyTurns, 0),
  pollingCalls: rows.reduce((a, r) => a + r.pollingCalls, 0),
}

const lines = []
lines.push('PHOENIX HARNESS V3 — PHASE 0 TELEMETRY FORENSICS')
lines.push(`corpus: ${files.length} session files, ${corpus.requests} model requests`)
lines.push(`tools per request (corpus): ${(corpus.toolResults / Math.max(1, corpus.requests)).toFixed(2)}`)
lines.push(`usage corpus: input=${corpus.usage.input} output=${corpus.usage.output} cacheRead=${corpus.usage.cacheRead} cacheWrite=${corpus.usage.cacheWrite} reasoning=${corpus.usage.reasoning}`)
lines.push(`billed-input-equivalent: ${corpus.billedInputEq} (cache-hit @0.1x) | cost-index(in+out): ${corpus.costIndex}`)
lines.push(`tool-result volume: ${corpus.toolResultChars} chars across ${corpus.toolResults} results`)
lines.push(`failures=${corpus.failures} repeatedOps=${corpus.repeatedOps} repeatedCalls=${corpus.repeatedCalls} bookkeepingOnlyTurns=${corpus.bookkeepingOnlyTurns} pollingCalls=${corpus.pollingCalls}`)
lines.push('')
for (const r of rows) {
  lines.push(`== ${r.id} ==`)
  lines.push(`  turns=${r.turns} requests=${r.requests} failures=${r.failures} ${JSON.stringify(r.failureCodes)}`)
  lines.push(`  tools/request=${r.toolsPerRequest} toolResults=${r.toolResults} toolChars=${r.toolResultChars} maxToolResult=${r.toolMaxChars}`)
  lines.push(`  input=${r.usage.input} output=${r.usage.output} cacheRead=${r.usage.cacheRead} reasoning=${r.usage.reasoning}`)
  lines.push(`  reqInput avg=${r.reqInputAvg} median=${r.reqInputMedian} p95=${r.reqInputP95} max=${r.reqInputMax} | estChars avg=${r.estInputCharsAvg} max=${r.estInputCharsMax}`)
  lines.push(`  billedInputEq=${r.billedInputEq} costIndex=${r.costIndex}`)
  lines.push(`  tiers=${JSON.stringify(r.tiers)} finishes=${JSON.stringify(r.finishes)}`)
  lines.push(`  repeatedOps=${r.repeatedOps} repeatedCalls=${r.repeatedCalls} bookkeepingOnlyTurns=${r.bookkeepingOnlyTurns} pollingCalls=${r.pollingCalls}`)
  for (const t of r.topTools) lines.push(`    tool ${t.tool}: ${t.calls} calls, ${t.chars} result chars`)
  for (const b of r.biggestResults) lines.push(`    biggest: ${b.tool} ${b.maxChars} chars`)
  for (const rep of r.topRepeats) lines.push(`    repeat ${rep.count}x: ${rep.preview}`)
  lines.push('')
}

process.stdout.write(lines.join('\n') + '\n')
if (outIdx >= 0) {
  const outPath = resolve(args[outIdx + 1])
  mkdirSync(dirname(outPath), { recursive: true })
  writeFileSync(outPath, JSON.stringify({ corpus, sessions: rows }, null, 2))
  console.error(`forensics written: ${outPath}`)
}
