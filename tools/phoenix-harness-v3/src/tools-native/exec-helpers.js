/**
 * Native-tool execution helpers (Lesson L-003: argument arrays only —
 * the model never composes shell quoting; L-006: env hygiene).
 * Every spawn is bounded (timeout, output caps) and fail-closed.
 */
import { execFile } from 'node:child_process'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { createHash } from 'node:crypto'

export const OUTPUT_CAP = 6000 // chars returned to the model
export const ARTIFACT_ROOT = '.phoenix-harness/v3-artifacts'

function pack(opts, ok, code, stdout, stderr) {
  const full = (stdout + (stderr ? `\n[stderr]\n${stderr}` : '')).trim()
  const truncated = full.length > OUTPUT_CAP
  const brief = truncated
    ? `${full.slice(0, Math.floor(OUTPUT_CAP * 0.7))}\n…[truncated ${full.length - OUTPUT_CAP} chars]\n${full.slice(-Math.floor(OUTPUT_CAP * 0.3))}`
    : full
  let artifact = null
  if (opts.writeArtifact !== false && full.length > 400) {
    try {
      const slug = `${new Date().toISOString().replace(/[:.]/g, '-')}-${opts.artifactTag ?? 'out'}.txt`
      const dir = join(process.cwd(), ARTIFACT_ROOT, opts.artifactDir ?? 'native')
      mkdirSync(dir, { recursive: true })
      const p = join(dir, slug)
      writeFileSync(p, full)
      artifact = p.replace(/\\/g, '/')
    } catch { artifact = null }
  }
  return { ok, code, stdout: brief, full, truncated, artifact }
}

/**
 * Run a command with an ARGS ARRAY (never a shell string).
 * Returns {ok, code, stdout, full, truncated, artifact} — never throws.
 */
export function run(cmd, args, opts = {}) {
  const timeoutMs = opts.timeoutMs ?? 60000
  const cwd = opts.cwd ?? process.cwd()
  return new Promise((resolveFn) => {
    let settled = false
    const settle = (r) => { if (!settled) { settled = true; resolveFn(r) } }
    try {
      execFile(cmd, args, {
        cwd,
        timeout: timeoutMs,
        maxBuffer: opts.maxBuffer ?? 4 * 1024 * 1024,
        windowsHide: true,
        env: { ...process.env },
      }, (error, stdout, stderr) => {
        if (error) settle(pack(opts, false, error.code ?? 1, String(stdout ?? ''), String(stderr ?? '')))
        else settle(pack(opts, true, 0, String(stdout ?? ''), String(stderr ?? '')))
      })
    } catch (err) {
      settle(pack(opts, false, 'ENOENT', '', String(err?.message ?? err)))
    }
  })
}

/** SHA-256 of a string (16 hex chars). */
export function sha16(s) {
  return createHash('sha256').update(String(s)).digest('hex').slice(0, 16)
}

/** Write a structured JSON artifact; returns repo-relative path or null. */
export function writeJsonArtifact(toolDir, tag, obj) {
  try {
    const slug = `${new Date().toISOString().replace(/[:.]/g, '-')}-${tag}.json`
    const dir = join(process.cwd(), ARTIFACT_ROOT, toolDir)
    mkdirSync(dir, { recursive: true })
    const p = join(dir, slug)
    writeFileSync(p, JSON.stringify(obj, null, 2))
    return p.replace(/\\/g, '/')
  } catch {
    return null
  }
}
