/**
 * Telemetry sink — append-only JSONL under .phoenix-harness/telemetry/.
 * V3 addition over V2: tool-result records carry an argument fingerprint
 * hash + preview so repeated operations are provable (Phase 0 limitation).
 * Every writer is incapable of throwing into the caller's path.
 */
import { appendFileSync, mkdirSync, readFileSync, readdirSync, existsSync } from 'node:fs'
import { join } from 'node:path'
import { createHash } from 'node:crypto'

export function nowIso() {
  return new Date().toISOString()
}

export function fingerprintArgs(raw) {
  try {
    let obj = raw
    if (typeof raw === 'string') {
      try { obj = JSON.parse(raw) } catch { return { fp: sha256(raw.slice(0, 4096)), preview: raw.slice(0, 120) } }
    }
    const sorted = JSON.stringify(sortDeep(obj ?? null))
    return { fp: sha256(sorted), preview: sorted.slice(0, 120) }
  } catch {
    return { fp: '(unfingerprintable)', preview: '(unfingerprintable)' }
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

function sha256(s) {
  return createHash('sha256').update(String(s)).digest('hex').slice(0, 16)
}

export function createTelemetrySink(root, options = {}) {
  const dir = join(root, '.phoenix-harness', 'telemetry')
  const maxRecords = options.maxRecords ?? 20000
  try { mkdirSync(dir, { recursive: true }) } catch { /* best-effort */ }

  function fileFor(sid) {
    return join(dir, `session-${String(sid).replace(/[^a-zA-Z0-9-]/g, '')}.jsonl`)
  }

  function record(sid, obj) {
    try {
      appendFileSync(fileFor(sid), JSON.stringify(obj) + '\n')
      return true
    } catch {
      return false
    }
  }

  /** Read records for one session ('*' = all files). */
  function readAll(sid) {
    const out = []
    try {
      const files = sid === '*' || sid === 'all'
        ? readdirSync(dir).filter((f) => f.endsWith('.jsonl'))
        : [basename(fileFor(sid))]
      for (const f of files) {
        const p = join(dir, f)
        if (!existsSync(p)) continue
        for (const line of readFileSync(p, 'utf8').split('\n')) {
          if (!line.trim()) continue
          try { out.push(JSON.parse(line)) } catch { /* skip torn line */ }
        }
      }
    } catch { /* read-only view, never throws */ }
    return out.slice(-maxRecords)
  }

  return { record, readAll, dir, fileFor }
}
