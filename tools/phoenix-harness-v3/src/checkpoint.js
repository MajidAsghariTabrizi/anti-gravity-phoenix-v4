/**
 * Durable session checkpoint (Layer F) — compact structured progress record
 * under .phoenix-harness/checkpoints/. get merges nothing; update merges
 * lists (dedup, capped) and replaces scalars. V3 addition: phase markers.
 */
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { join } from 'node:path'

const LIST_KEYS = ['known', 'unknown', 'hypotheses', 'decisions', 'filesChanged', 'testsRun', 'blockers']
const SCALAR_KEYS = ['objective', 'nextAction']
const LIST_CAP = 60

export function checkpointPath(root, sid) {
  return join(root, '.phoenix-harness', 'checkpoints', `session-${String(sid).replace(/[^a-zA-Z0-9-]/g, '')}.json`)
}

export function readCheckpoint(root, sid) {
  const p = checkpointPath(root, sid)
  try {
    if (!existsSync(p)) return { exists: false, path: p, data: {} }
    return { exists: true, path: p, data: JSON.parse(readFileSync(p, 'utf8')) }
  } catch {
    return { exists: false, path: p, data: {} }
  }
}

function dedupe(list) {
  return [...new Set((Array.isArray(list) ? list : []).map((s) => String(s)).filter(Boolean))].slice(-LIST_CAP)
}

export function mergeCheckpoint(current, update = {}) {
  const base = current.data ?? {}
  const out = { ...base, version: 1, updatedAt: new Date().toISOString() }
  for (const k of SCALAR_KEYS) {
    if (update[k] !== undefined) {
      out[k] = update[k] === null ? '' : String(update[k])
    }
  }
  for (const k of LIST_KEYS) {
    if (update[k] !== undefined) {
      out[k] = update.replace === true ? dedupe(update[k]) : dedupe([...(base[k] ?? []), ...(update[k] ?? [])])
    }
  }
  if (update.phase !== undefined) out.phase = String(update.phase)
  return out
}

export function writeCheckpoint(root, sid, data) {
  const p = checkpointPath(root, sid)
  try {
    mkdirSync(join(root, '.phoenix-harness', 'checkpoints'), { recursive: true })
    const text = JSON.stringify(data, null, 2)
    writeFileSync(p, text)
    return { ok: true, path: p, chars: text.length }
  } catch (err) {
    return { ok: false, path: p, error: String(err?.message ?? err) }
  }
}

export function renderCheckpoint(c) {
  const d = c.data ?? {}
  const lines = []
  if (d.objective) lines.push(`objective: ${d.objective}`)
  if (d.phase) lines.push(`phase: ${d.phase}`)
  const label = {
    known: 'KNOWN', unknown: 'UNKNOWN', hypotheses: 'HYPOTHESES', decisions: 'DECISIONS',
    filesChanged: 'FILES CHANGED', testsRun: 'TESTS RUN', blockers: 'BLOCKERS',
  }
  for (const k of LIST_KEYS) {
    lines.push(`${label[k]} (${(d[k] ?? []).length}):`)
    for (const item of d[k] ?? []) lines.push(`- ${item}`)
  }
  if (d.nextAction) lines.push(`NEXT: ${d.nextAction}`)
  lines.push(`updatedAt: ${d.updatedAt ?? '(never)'}`)
  return lines.join('\n')
}
