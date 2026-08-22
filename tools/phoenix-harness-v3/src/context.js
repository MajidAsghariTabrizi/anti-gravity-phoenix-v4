/**
 * Layered context compiler (Phase 4) — retrieval over replay.
 * Layers:
 *   A stable kernel      -> knowledge/kernel.md
 *   B MissionSpec        -> mission.js (phoenix_mission)
 *   C domain packs       -> .phoenix-harness/domains/*.md
 *   D working set        -> transcript + compaction policy (preset)
 *   E fresh evidence     -> tool results (bounded by tool-result-pruner)
 *   F durable checkpoint -> checkpoint.js (phoenix_checkpoint)
 *
 * phoenix_context serves A/C plus maps/registries on demand and reports
 * context pressure from telemetry (estInputChars of recent requests).
 */
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs'
import { join, resolve, sep } from 'node:path'

const MAX_ARTIFACT_CHARS = 24000

export function createContextServer(root, knowledgeRoot) {
  const harnessRoot = join(root, '.phoenix-harness')
  const kRoot = resolve(knowledgeRoot)

  function safePath(base, rel) {
    const p = resolve(base, rel)
    if (!p.startsWith(resolve(base) + sep) && p !== resolve(base)) return null
    return p
  }

  function listAll() {
    const out = { harness: [], knowledge: [] }
    try {
      for (const f of readdirSync(harnessRoot)) {
        if (f.endsWith('.json') || f.endsWith('.md')) out.harness.push(f)
      }
      for (const f of readdirSync(join(harnessRoot, 'domains'))) {
        if (f.endsWith('.md')) out.harness.push(`domains/${f}`)
      }
    } catch { /* best-effort */ }
    try {
      const walk = (d, prefix) => {
        for (const e of readdirSync(d, { withFileTypes: true })) {
          const p = join(d, e.name)
          if (e.isDirectory()) walk(p, `${prefix}${e.name}/`)
          else if (e.name.endsWith('.md') || e.name.endsWith('.json')) out.knowledge.push(`${prefix}${e.name}`)
        }
      }
      walk(kRoot, 'knowledge/')
    } catch { /* best-effort */ }
    return out
  }

  function loadFile(file) {
    const candidates = [safePath(harnessRoot, file), safePath(kRoot, String(file).replace(/^knowledge[\\/]/, ''))]
    for (const p of candidates) {
      if (p && existsSync(p)) {
        try {
          if (statSync(p).isDirectory()) return { error: `"${file}" is a directory` }
          const text = readFileSync(p, 'utf8')
          const truncated = text.length > MAX_ARTIFACT_CHARS
          return { name: file, text: truncated ? text.slice(0, MAX_ARTIFACT_CHARS) : text, truncated: truncated ? text.length - MAX_ARTIFACT_CHARS : 0 }
        } catch (err) {
          return { error: String(err?.message ?? err) }
        }
      }
    }
    return { error: `no such artifact "${file}"` }
  }

  function searchArtifacts(query) {
    const q = String(query ?? '').toLowerCase()
    if (!q) return []
    const hits = []
    const scan = (base, prefix) => {
      try {
        for (const e of readdirSync(base, { withFileTypes: true })) {
          const p = join(base, e.name)
          if (e.isDirectory()) scan(p, `${prefix}${e.name}/`)
          else if (e.name.endsWith('.md') || e.name.endsWith('.json')) {
            const text = readFileSync(p, 'utf8')
            text.split('\n').forEach((line, i) => {
              if (line.toLowerCase().includes(q)) hits.push({ name: `${prefix}${e.name}`, line: i + 1, text: line.trim().slice(0, 160) })
            })
          }
          if (hits.length >= 40) return
        }
      } catch { /* best-effort */ }
    }
    scan(harnessRoot, '')
    if (hits.length < 40) scan(kRoot, 'knowledge/')
    return hits.slice(0, 40)
  }

  /** Context-pressure view from recent telemetry estInputChars. */
  function pressureView(telemetryRecords) {
    const recent = telemetryRecords.filter((r) => r.event === 'llm.request' && r.estInputChars).slice(-12)
    if (recent.length === 0) return { verdict: 'NORMAL', estContextChars: null, targets: null, advice: 'no telemetry yet' }
    const avg = Math.round(recent.reduce((a, r) => a + r.estInputChars, 0) / recent.length)
    const max = Math.max(...recent.map((r) => r.estInputChars))
    const targets = { normal: [30000, 70000], p95: 96000, hardLimit: 160000, pressureThreshold: [96000, 120000], retainTail: 32768, compactionSummaryTokens: [4000, 6000] }
    let verdict = 'NORMAL'
    let advice = 'context within targets'
    if (avg > 120000) { verdict = 'LIMIT'; advice = 'at/above pressure band: compact now (command-compact), stop large reads, use phoenix_context instead of re-reading files' }
    else if (avg > 96000) { verdict = 'PRESSURE'; advice = 'in pressure band: prefer artifact refs, avoid whole-file reads, compact at next phase boundary' }
    else if (avg > 70000) { verdict = 'ELEVATED'; advice = 'above normal target: trim tool results, prefer focused reads' }
    return { verdict, estContextChars: avg, recentMax: max, targets, advice }
  }

  return { listAll, loadFile, searchArtifacts, pressureView }
}
