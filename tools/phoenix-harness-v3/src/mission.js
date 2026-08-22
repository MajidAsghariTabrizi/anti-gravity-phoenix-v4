/**
 * Mission compiler (Phase 3) — user intent -> typed MissionSpec.
 * The MissionSpec is the single mission source: durable under
 * .phoenix-harness/checkpoints/mission-<session>.json, retrieved at mission
 * start and phase boundaries, NEVER repeated inside prompts.
 */
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { join } from 'node:path'

export const RISK_TIERS = ['local_only', 'prod_readonly', 'prod_mutation']
export const HARD_STOPS = [
  'budget_breach', 'safety_breach', 'evidence_missing', 'staleness', 'uncertain_submission',
]
const KNOWN_DOMAINS = [
  'live-execution', 'engine', 'ingestion-feed', 'rpc-provider', 'observers',
  'economic-accounting', 'chain-reconciliation', 'signer-finality', 'contracts',
  'release-deployment', 'ci-build', 'operations-diagnostics', 'database-state', 'business',
]

export function missionPath(root, sid) {
  return join(root, '.phoenix-harness', 'checkpoints', `mission-${String(sid).replace(/[^a-zA-Z0-9-]/g, '')}.json`)
}

export function readMission(root, sid) {
  const p = missionPath(root, sid)
  try {
    if (!existsSync(p)) return { exists: false, path: p, spec: null }
    return { exists: true, path: p, spec: JSON.parse(readFileSync(p, 'utf8')) }
  } catch {
    return { exists: false, path: p, spec: null }
  }
}

export function writeMission(root, sid, spec) {
  const p = missionPath(root, sid)
  try {
    mkdirSync(join(root, '.phoenix-harness', 'checkpoints'), { recursive: true })
    const text = JSON.stringify(spec, null, 2)
    writeFileSync(p, text)
    return { ok: true, path: p, chars: text.length }
  } catch (err) {
    return { ok: false, path: p, error: String(err?.message ?? err) }
  }
}

export const DEFAULTS = {
  tokenBudget: 8_000_000, // billed-equivalent tokens (mission-scale default)
  modelCallBudget: 400,
  elapsedMinutes: 480,
  contextTargetTokens: 60_000,
}

/**
 * Validate + compile a MissionSpec from tool input. Returns {spec} or
 * {error}. Never throws.
 */
export function compileMission(input = {}, now = new Date().toISOString()) {
  const objective = String(input.objective ?? '').trim()
  if (!objective) return { error: 'objective is required' }
  if (objective.length > 600) return { error: 'objective too long (max 600 chars)' }

  const riskTier = RISK_TIERS.includes(input.riskTier) ? input.riskTier : 'local_only'
  const domains = [...new Set((Array.isArray(input.domains) ? input.domains : [])
    .map((d) => String(d).toLowerCase().trim()))]
  const badDomain = domains.find((d) => !KNOWN_DOMAINS.includes(d))
  if (badDomain) return { error: `unknown domain "${badDomain}"; known: ${KNOWN_DOMAINS.join(', ')}` }
  if (domains.length === 0) domains.push('business')

  const posInt = (v, dflt, max) => {
    const n = Number(v)
    if (!Number.isFinite(n) || n <= 0) return dflt
    return Math.min(Math.round(n), max)
  }
  const hardStops = [...new Set((Array.isArray(input.hardStops) ? input.hardStops : ['budget_breach'])
    .map((h) => String(h).toLowerCase()))]
    .filter((h) => HARD_STOPS.includes(h))
  if (hardStops.length === 0) hardStops.push('budget_breach')

  const spec = {
    schema: 1,
    createdAt: now,
    updatedAt: now,
    objective,
    businessObjective: String(input.businessObjective ?? '').slice(0, 300) || null,
    technicalObjective: String(input.technicalObjective ?? '').slice(0, 300) || null,
    domains,
    riskTier,
    authority: riskTier === 'prod_mutation' ? 'owner-required' : (input.authority ?? 'agent-local'),
    acceptanceCriteria: (Array.isArray(input.acceptanceCriteria) ? input.acceptanceCriteria : [])
      .map((a) => String(a).slice(0, 200)).filter(Boolean).slice(0, 8),
    evidenceRequirements: (Array.isArray(input.evidenceRequirements) ? input.evidenceRequirements : [])
      .map((e) => String(e).slice(0, 160)).filter(Boolean).slice(0, 8),
    budgets: {
      tokens: posInt(input.tokenBudget, DEFAULTS.tokenBudget, 100_000_000),
      modelCalls: posInt(input.modelCallBudget, DEFAULTS.modelCallBudget, 5000),
      elapsedMinutes: posInt(input.elapsedBudgetMinutes, DEFAULTS.elapsedMinutes, 7 * 24 * 60),
    },
    hardStops,
    derived: {
      contextTargetTokens: DEFAULTS.contextTargetTokens,
      tier: riskTier === 'prod_mutation' || domains.some((d) => ['live-execution', 'economic-accounting', 'signer-finality', 'release-deployment'].includes(d))
        ? 'critical' : 'standard',
    },
    ownerApproval: null, // {approvedAt, by, scope} — required before prod_mutation dispatch
    phases: [],
  }
  return { spec }
}

export function renderMission(spec) {
  const lines = [
    'MISSION SPEC (durable — never repeat in prompts)',
    `objective: ${spec.objective}`,
    `business:  ${spec.businessObjective ?? '(unset)'}`,
    `technical: ${spec.technicalObjective ?? '(unset)'}`,
    `domains:   ${spec.domains.join(', ')}`,
    `riskTier:  ${spec.riskTier} | authority: ${spec.authority} | tier: ${spec.derived.tier}`,
    `budgets:   ${spec.budgets.tokens.toLocaleString()} tokens | ${spec.budgets.modelCalls} model calls | ${spec.budgets.elapsedMinutes} min`,
    `hardStops: ${spec.hardStops.join(', ')}`,
    `acceptance (${spec.acceptanceCriteria.length}):`,
    ...spec.acceptanceCriteria.map((a, i) => `  ${i + 1}. ${a}`),
    `evidence (${spec.evidenceRequirements.length}):`,
    ...spec.evidenceRequirements.map((e, i) => `  ${i + 1}. ${e}`),
    `ownerApproval: ${spec.ownerApproval ? `GRANTED ${spec.ownerApproval.approvedAt} (${spec.ownerApproval.scope})` : 'none (required for prod_mutation)'}`,
    `phases: ${spec.phases.length ? spec.phases.map((p) => `${p.name}@${p.at}`).join(' -> ') : '(none marked)'}`,
    '',
    'Actions: phoenix_mission get | update {phase, ...} | owner_approval {scope} | close',
  ]
  return lines.join('\n')
}

/** Mark a phase boundary (context-compiler trigger). */
export function markPhase(root, sid, name) {
  const cur = readMission(root, sid)
  if (!cur.exists) return { error: 'no mission exists; create first' }
  const spec = cur.spec
  spec.updatedAt = new Date().toISOString()
  spec.phases = [...(spec.phases ?? []), { name: String(name).slice(0, 40), at: spec.updatedAt }].slice(-12)
  const written = writeMission(root, sid, spec)
  return written.ok ? { spec } : { error: written.error }
}
