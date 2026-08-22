/**
 * Phoenix Harness V3 eval — telemetry parsing for gates (Phase 4).
 *
 * Parses V2/V3 session JSONL sinks (identical event contracts:
 * llm.request usage {input,output,cacheRead,cacheWrite,reasoning},
 * estInputChars, llm.failure, tool.result {tool,chars}) into per-session
 * aggregates used by the cost/context/reliability gates.
 */
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs'
import { join } from 'node:path'

export const CACHE_HIT_DISCOUNT = 0.1
export const BOOKKEEPING_TOOLS = new Set(['get_goal', 'update_goal', 'phoenix_checkpoint', 'job_list', 'list_agents', 'create_goal'])
export const POLLING_TOOLS = new Set(['job_output', 'job_list', 'list_agents'])

export function newSessionStat(id) {
  return {
    id, lines: 0, requests: 0, failures: 0, failureCodes: {},
    toolResults: 0, toolChars: 0, tools: {},
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 },
    estInputChars: [], reqInputTokens: [],
    pollingCalls: 0, bookkeepingCalls: 0,
  }
}

export function parseLine(line, sessions) {
  let obj
  try { obj = JSON.parse(line) } catch { return }
  if (!obj || !obj.event) return
  const sid = obj.session ?? '(unknown)'
  let s = sessions.get(sid)
  if (!s) { s = newSessionStat(sid); sessions.set(sid, s) }
  s.lines += 1
  if (obj.event === 'llm.request') {
    s.requests += 1
    const u = obj.usage ?? {}
    s.usage.input += u.input ?? 0
    s.usage.output += u.output ?? 0
    s.usage.cacheRead += u.cacheRead ?? 0
    s.usage.cacheWrite += u.cacheWrite ?? 0
    s.usage.reasoning += u.reasoning ?? 0
    s.reqInputTokens.push((u.input ?? 0) + (u.cacheRead ?? 0))
    if (obj.estInputChars !== undefined && obj.estInputChars !== null) s.estInputChars.push(obj.estInputChars)
    if (obj.failure) {
      s.failures += 1
      const code = typeof obj.failure === 'string' ? obj.failure : (obj.failure.code ?? obj.failure.type ?? '(object)')
      s.failureCodes[code] = (s.failureCodes[code] ?? 0) + 1
    }
  } else if (obj.event === 'llm.failure') {
    s.failures += 1
    s.failureCodes[obj.code ?? '(unknown)'] = (s.failureCodes[obj.code ?? '(unknown)'] ?? 0) + 1
  } else if (obj.event === 'tool.result') {
    s.toolResults += 1
    s.toolChars += obj.chars ?? 0
    const name = obj.tool ?? '(unknown)'
    s.tools[name] = (s.tools[name] ?? 0) + 1
    if (POLLING_TOOLS.has(name)) s.pollingCalls += 1
    if (BOOKKEEPING_TOOLS.has(name)) s.bookkeepingCalls += 1
  }
}

export function parseTelemetryFile(file) {
  const sessions = new Map()
  try {
    const text = readFileSync(file, 'utf8')
    for (const line of text.split('\n')) if (line.trim()) parseLine(line, sessions)
  } catch { /* unreadable */ }
  return sessions
}

export function parseTelemetryDir(dir) {
  const sessions = new Map()
  if (!existsSync(dir)) return sessions
  const walk = (base, depth) => {
    if (depth > 3) return
    let entries
    try { entries = readdirSync(base, { withFileTypes: true }) } catch { return }
    for (const e of entries) {
      const p = join(base, e.name)
      if (e.isDirectory()) { walk(p, depth + 1); continue }
      if (!e.name.endsWith('.jsonl') && !e.name.endsWith('.json')) continue
      if (e.name.startsWith('session-') || e.name.startsWith('eval-')) {
        for (const [sid, s] of parseTelemetryFile(p)) sessions.set(sid, s)
      }
    }
  }
  walk(dir, 0)
  return sessions
}

/** Merge session stats into one aggregate. */
export function aggregate(sessions) {
  const out = newSessionStat('aggregate')
  for (const s of sessions.values()) {
    out.lines += s.lines
    out.requests += s.requests
    out.failures += s.failures
    for (const [k, v] of Object.entries(s.failureCodes)) out.failureCodes[k] = (out.failureCodes[k] ?? 0) + v
    out.toolResults += s.toolResults
    out.toolChars += s.toolChars
    for (const [k, v] of Object.entries(s.tools)) out.tools[k] = (out.tools[k] ?? 0) + v
    for (const k of Object.keys(out.usage)) out.usage[k] += s.usage[k]
    out.estInputChars.push(...s.estInputChars)
    out.reqInputTokens.push(...s.reqInputTokens)
    out.pollingCalls += s.pollingCalls
    out.bookkeepingCalls += s.bookkeepingCalls
  }
  return out
}

export function pct(list, p) {
  if (!list || list.length === 0) return null
  const sorted = [...list].sort((a, b) => a - b)
  const idx = Math.min(sorted.length - 1, Math.floor(sorted.length * p))
  return sorted[idx]
}

export function median(list) {
  return pct(list, 0.5)
}

/** billedEq = input + 0.1*cacheRead + output (documented assumption). */
export function billedEq(usage) {
  return Math.round(usage.input + CACHE_HIT_DISCOUNT * usage.cacheRead + usage.output)
}
