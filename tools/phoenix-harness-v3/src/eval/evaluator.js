/**
 * Evaluator (Phase 8) — proof-carrying completion certificates.
 *
 * A certificate binds a gate verdict to the exact evidence files via
 * SHA-256 hashes. verifyCertificate re-hashes every cited artifact, so a
 * certificate is only valid with its evidence intact (proof-carrying).
 * Reviewers receive only MissionSpec + evidence + result paths — never
 * the parent transcript.
 */
import { createHash } from 'node:crypto'
import { readFileSync, existsSync, writeFileSync, mkdirSync } from 'node:fs'
import { join, resolve } from 'node:path'

export const GATES = ['business', 'architecture', 'prod_safety', 'release', 'evidence']

export function hashFile(p) {
  return createHash('sha256').update(readFileSync(p)).digest('hex')
}

/**
 * @param {object} c {gate, task, mission, verdict, reviewer, evidence[] (file paths), notes}
 */
export function makeCertificate(c) {
  const evidence = (c.evidence ?? []).map((p) => {
    const abs = resolve(p)
    if (!existsSync(abs)) throw new Error(`evidence missing: ${p}`)
    return { path: p, sha256: hashFile(abs) }
  })
  const cert = {
    schema: 'phoenix.certificate.v1',
    createdAt: new Date().toISOString(),
    gate: c.gate,
    task: c.task,
    mission: c.mission ?? null,
    verdict: c.verdict, // pass | fail | blocked
    reviewer: c.reviewer ?? 'unset',
    notes: String(c.notes ?? '').slice(0, 500),
    evidence,
  }
  cert.proof = createHash('sha256').update(JSON.stringify({
    gate: cert.gate, task: cert.task, verdict: cert.verdict, reviewer: cert.reviewer,
    evidence: evidence.map((e) => `${e.path}:${e.sha256}`),
  })).digest('hex')
  return cert
}

export function writeCertificate(dir, cert) {
  mkdirSync(dir, { recursive: true })
  const p = join(dir, `cert-${cert.gate}-${cert.proof.slice(0, 12)}.json`)
  writeFileSync(p, JSON.stringify(cert, null, 2))
  return p
}

export function verifyCertificate(cert) {
  const problems = []
  if (!GATES.includes(cert.gate)) problems.push(`unknown gate ${cert.gate}`)
  if (!['pass', 'fail', 'blocked'].includes(cert.verdict)) problems.push(`bad verdict ${cert.verdict}`)
  for (const e of cert.evidence ?? []) {
    try {
      const p = resolve(e.path)
      if (!existsSync(p)) { problems.push(`evidence missing: ${e.path}`); continue }
      if (hashFile(p) !== e.sha256) problems.push(`evidence hash mismatch: ${e.path}`)
    } catch (err) {
      problems.push(`evidence unreadable: ${e.path} (${err.message})`)
    }
  }
  const recomputed = createHash('sha256').update(JSON.stringify({
    gate: cert.gate, task: cert.task, verdict: cert.verdict, reviewer: cert.reviewer,
    evidence: (cert.evidence ?? []).map((e) => `${e.path}:${e.sha256}`),
  })).digest('hex')
  if (recomputed !== cert.proof) problems.push('certificate proof mismatch (tampered)')
  return { valid: problems.length === 0, problems }
}
