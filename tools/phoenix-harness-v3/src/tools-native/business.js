/**
 * phoenix_ground_truth / phoenix_business_funnel /
 * phoenix_opportunity_replay / phoenix_evidence — business-twin tools.
 *
 * Label discipline (never mix): expected | conservative | shadow | realized.
 * Realized comes ONLY from reconciliation/ledger entries. Every figure
 * carries its source path + as-of date (freshness-policy).
 */
import { readFileSync, readdirSync, existsSync, writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
import { sha16, writeJsonArtifact } from './exec-helpers.js'

const EVIDENCE_DIR = '.phoenix-harness/evidence/claims'

function readJson(p) {
  try { return JSON.parse(readFileSync(p, 'utf8')) } catch { return null }
}

function ledgers(root) {
  return {
    platform: {
      file: 'docs/evidence/platform-transition-20260802/ECONOMIC_LEDGER.json',
      summary: (d) => ({
        schema: d?.schema, generatedAt: d?.generated_at, repositorySha: d?.repository_sha,
        activeReleaseSha: d?.active_release_sha, entries: (d?.entries ?? []).length,
        realizedNetPnl: d?.realized_net_pnl ?? null, completeness: d?.completeness,
        unknowns: (d?.unknowns ?? []).slice(0, 4),
      }),
    },
    a1: {
      file: 'fixtures/hunter-a1/v1/revenue-replay-evidence.json',
      summary: (d) => ({
        schema: d?.schema_version, evidenceClass: d?.evidence_class,
        eventsProcessed: d?.events_processed, qualifiedCandidates: d?.qualified_candidates,
        positiveConservativeNetPnl: d?.positive_conservative_net_pnl ?? [],
        p50: d?.positive_conservative_net_pnl_p50, p95: d?.positive_conservative_net_pnl_p95,
        note: 'fixture replay evidence — conservative labels only; NOT realized revenue',
      }),
    },
    missing_alpha: {
      file: '.agent-private/alpha-source-investigation/MISSING_ALPHA_LEDGER.md',
      summary: (text) => ({ text: String(text).split('\n').filter((l) => l.trim()).slice(0, 30).join('\n').slice(0, 3000) }),
    },
  }
}

export function groundTruthTool(root) {
  return {
    name: 'phoenix_ground_truth',
    description:
      'Read a Ground-Truth delivery ledger and return a compact structured summary with source path + as-of date. ledgers: platform (ECONOMIC_LEDGER), a1 (hunter-a1 fixture replay), missing_alpha (MISSING_ALPHA_LEDGER), list (index). Profit labels are preserved exactly as labeled in the source — expected/conservative/shadow are NEVER reported as realized.',
    parameters: {
      ledger: { type: 'string', required: true, description: 'platform | a1 | missing_alpha | list' },
    },
    execute: async (args) => {
      const L = ledgers(root)
      if (args.ledger === 'list') {
        return Object.entries(L).map(([k, v]) => `${k}: ${v.file}`).join('\n')
      }
      const entry = L[String(args.ledger ?? '')]
      if (!entry) return `error: unknown ledger "${args.ledger}" (platform | a1 | missing_alpha | list)`
      const p = join(root, ...entry.file.split('/'))
      if (!existsSync(p)) return `error: ledger file missing: ${entry.file}`
      const raw = readFileSync(p, 'utf8')
      let summary
      if (entry.file.endsWith('.json')) {
        const d = readJson(p)
        summary = entry.summary(d ?? { parseError: true })
      } else {
        summary = entry.summary(raw)
      }
      const artifact = writeJsonArtifact('ground_truth', String(args.ledger), { ledger: entry.file, summary })
      return [`GROUND TRUTH — ${entry.file}`, JSON.stringify(summary, null, 2).slice(0, 3500), artifact ? `artifact: ${artifact}` : ''].filter(Boolean).join('\n')
    },
  }
}

export function businessFunnelTool(root, knowledgeRoot) {
  const TWIN = join(knowledgeRoot, 'business-twin.md')
  return {
    name: 'phoenix_business_funnel',
    description:
      'Answer one business-twin question with evidence from knowledge/business-twin.md (every figure carries source + as-of date). Questions: revenue_why_zero | noalpha_vs_gap | addressable | ev_per_day | funnel | asset_route_size | pnl_labels | competitor_missed | next_move | all. Facts are never invented; UNKNOWN is reported as UNKNOWN.',
    parameters: {
      question: { type: 'string', required: true, description: 'revenue_why_zero | noalpha_vs_gap | addressable | ev_per_day | funnel | asset_route_size | pnl_labels | competitor_missed | next_move | all' },
    },
    execute: async (args) => {
      if (!existsSync(TWIN)) return 'error: business-twin.md not built yet'
      const text = readFileSync(TWIN, 'utf8')
      const q = String(args.question ?? 'all')
      const sections = text.split(/^## /m).slice(1).map((s) => ({ title: s.split('\n')[0].trim(), body: s }))
      const wanted = {
        revenue_why_zero: /why.*zero/i, noalpha_vs_gap: /no.?alpha/i, addressable: /addressable/i,
        ev_per_day: /engineering.?day|EV/i, funnel: /funnel/i, asset_route_size: /asset.*route.*size|route.*size/i,
        pnl_labels: /expected.*conservative.*realized|PnL/i, competitor_missed: /competitor|missed/i,
        next_move: /next.*move|highest/i,
      }
      const picked = q === 'all' ? sections : sections.filter((s) => wanted[q]?.test(s.title))
      if (!picked.length) return `error: no section for "${q}" (${Object.keys(wanted).join(' | ')} | all)`
      const body = picked.map((s) => `## ${s.title}\n${s.body.split('\n').slice(1).join('\n')}`).join('\n')
      return body.slice(0, 5500) + (body.length > 5500 ? `\n…[truncated; full file: ${TWIN}]` : '')
    },
  }
}

export function opportunityReplayTool(root) {
  return {
    name: 'phoenix_opportunity_replay',
    description:
      'Replay opportunity evidence records (a1 fixture replay + economic ledger entries) as a compact opportunity table: source, event count, candidates, conservative PnL values, realized PnL. Labels preserved exactly; conservative/shadow never presented as realized.',
    parameters: {
      source: { type: 'string', description: 'a1 | platform | all (default all)' },
    },
    execute: async (args) => {
      const src = String(args.source ?? 'all')
      const rows = []
      if (src === 'all' || src === 'a1') {
        const p = join(root, 'fixtures', 'hunter-a1', 'v1', 'revenue-replay-evidence.json')
        if (existsSync(p)) {
          const d = readJson(p)
          rows.push({
            source: 'a1-fixture-replay', evidenceClass: d?.evidence_class,
            events: d?.events_processed, candidates: d?.qualified_candidates,
            conservativeNetPnl: d?.positive_conservative_net_pnl ?? [],
            realizedNetPnl: 'NOT APPLICABLE (fixture replay, not a submission)',
            asOf: 'fixture commit snapshot',
          })
        }
      }
      if (src === 'all' || src === 'platform') {
        const p = join(root, 'docs', 'evidence', 'platform-transition-20260802', 'ECONOMIC_LEDGER.json')
        if (existsSync(p)) {
          const d = readJson(p)
          rows.push({
            source: 'platform-economic-ledger', generatedAt: d?.generated_at,
            entries: (d?.entries ?? []).length, realizedNetPnl: d?.realized_net_pnl ?? 0,
            completeness: d?.completeness, unknowns: (d?.unknowns ?? []).slice(0, 2),
          })
        }
      }
      if (rows.length === 0) return `error: no replay evidence for "${src}"`
      const artifact = writeJsonArtifact('opportunity_replay', src, rows)
      const lines = ['OPPORTUNITY REPLAY (labels as-labeled in sources)', ...rows.map((r) => JSON.stringify(r, null, 2))]
      if (artifact) lines.push(`artifact: ${artifact}`)
      return lines.join('\n').slice(0, 4500)
    },
  }
}

export function evidenceTool(root) {
  const FRESHNESS = { 'prod-live': 5 * 60000, chain: 15 * 60000, release: 0, ci: 5 * 60000, ledger: null, market: 60 * 60000, repo: 0 }
  function claimsDir() {
    const d = join(root, EVIDENCE_DIR)
    mkdirSync(d, { recursive: true })
    return d
  }
  return {
    name: 'phoenix_evidence',
    description:
      'Evidence registry for claims. register: store a claim {claim, kind FACT|HYPOTHESIS|PROPOSAL|UNKNOWN, sources[], freshnessClass} and return its id. get: read a claim with freshness verdict (STALE/FRESH per freshness-policy). list: index. verify: re-check freshness of all claims. Claims are hashed (proof-carrying for eval certificates).',
    parameters: {
      action: { type: 'string', required: true, description: 'register | get | list | verify' },
      claim: { type: 'string', description: 'claim text (register)' },
      kind: { type: 'string', description: 'FACT | HYPOTHESIS | PROPOSAL | UNKNOWN' },
      sources: { type: 'array', items: { type: 'string' }, description: 'source paths with as-of dates' },
      freshnessClass: { type: 'string', description: 'prod-live | chain | release | ci | ledger | market | repo' },
      id: { type: 'string', description: 'claim id (get)' },
    },
    execute: async (args) => {
      const dir = claimsDir()
      if (args.action === 'register') {
        const claim = String(args.claim ?? '').trim()
        if (!claim) return 'error: claim required'
        if (claim.length > 800) return 'error: claim too long'
        const kind = ['FACT', 'HYPOTHESIS', 'PROPOSAL', 'UNKNOWN'].includes(String(args.kind ?? '')) ? String(args.kind) : 'UNKNOWN'
        const sources = (Array.isArray(args.sources) ? args.sources : []).map(String).slice(0, 8)
        const cls = FRESHNESS[String(args.freshnessClass ?? 'ledger')] !== undefined ? String(args.freshnessClass) : 'ledger'
        const id = `${kind.toLowerCase()}-${sha16(claim + JSON.stringify(sources))}`
        const rec = { id, claim, kind, sources, freshnessClass: cls, registeredAt: new Date().toISOString(), hash: sha16(claim + JSON.stringify(sources) + 'phoenix-evidence-v1') }
        writeFileSync(join(dir, `${id}.json`), JSON.stringify(rec, null, 2))
        return `CLAIM REGISTERED: ${id}\n${JSON.stringify(rec, null, 2)}`
      }
      if (args.action === 'get') {
        const id = String(args.id ?? '')
        const p = join(dir, `${id}.json`)
        if (!/^[a-z]+-[0-9a-f]{16}$/.test(id) || !existsSync(p)) return `error: unknown claim id "${id}"`
        const rec = JSON.parse(readFileSync(p, 'utf8'))
        const verdict = freshnessVerdict(rec, FRESHNESS)
        return `${JSON.stringify(rec, null, 2)}\nfreshness: ${verdict}`
      }
      if (args.action === 'list') {
        const ids = readdirSync(dir).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort()
        return `claims (${ids.length}):\n${ids.join('\n')}`
      }
      if (args.action === 'verify') {
        const out = []
        for (const f of readdirSync(dir).filter((x) => x.endsWith('.json'))) {
          const rec = JSON.parse(readFileSync(join(dir, f), 'utf8'))
          out.push({ id: rec.id, freshness: freshnessVerdict(rec, FRESHNESS), kind: rec.kind, claim: rec.claim.slice(0, 100) })
        }
        return `EVIDENCE VERIFY (${out.length} claims):\n${out.map((o) => `${o.freshness.padEnd(6)} ${o.id} [${o.kind}] ${o.claim}`).join('\n')}`
      }
      return 'error: action must be register | get | list | verify'
    },
  }
}

function freshnessVerdict(rec, FRESHNESS) {
  const max = FRESHNESS[rec.freshnessClass]
  if (max === null || max === undefined) return rec.freshnessClass === 'ledger' ? 'HISTORICAL' : 'OK'
  const age = Date.now() - Date.parse(rec.registeredAt)
  if (max === 0) return 'REFRESH-BEFORE-USE'
  return age <= max ? 'FRESH' : 'STALE'
}
