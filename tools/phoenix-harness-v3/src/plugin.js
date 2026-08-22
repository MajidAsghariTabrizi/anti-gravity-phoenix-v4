/**
 * @phoenix/dsh-phoenix-harness-v3 — Phoenix Intelligence OS preset plugin.
 *
 * One preset-local plugin (zero external dependencies; node builtins only):
 *  1. reasoning-tier observation at agent/pre-step (V2-validated contract);
 *  2. per-request telemetry via llm/stream (usage, cache buckets, finish,
 *     failure) — CONTRACT: listener returns AsyncIterable DIRECTLY, never
 *     a Promise (Lesson L-001, regression test required);
 *  3. storm + loop detection on agent/request-error and tools/result
 *     (V3: tool.result records carry argument fingerprints);
 *  4. round/budget governor: usage accumulation, wait registry, no-op
 *     round rejection at agent/pre-step ({kind:'reject'} = zero-cost
 *     blocked turn), budget warnings/verdicts;
 *  5. durable MissionSpec (phoenix_mission) + checkpoints (phoenix_checkpoint);
 *  6. layered context retrieval (phoenix_context) + budget view
 *     (phoenix_budget) + telemetry view (phoenix_telemetry);
 *  7. the 15 typed fail-closed native tools (tools-native/*).
 *
 * Every listener is defensive: a failure here can never break a request,
 * a tool call, or the agent loop.
 */
import { classify, latestUserText, estimateInputChars } from './tier.js'
import { createFingerprintTracker, createStormTracker, resultContentChars } from './guard.js'
import { createTelemetrySink, nowIso, fingerprintArgs } from './sink.js'
import { createContextServer } from './context.js'
import { readCheckpoint, mergeCheckpoint, writeCheckpoint, renderCheckpoint } from './checkpoint.js'
import { compileMission, renderMission, readMission, writeMission, markPhase } from './mission.js'
import { createGovernor } from './governor.js'
import { compileParameterSchema } from './schema.js'
import { repoSnapshotTool, symbolTool } from './tools-native/repo.js'
import { testTool } from './tools-native/test.js'
import { remoteTool, productionSnapshotTool, sqlReadonlyTool, releaseVerifyTool } from './tools-native/remote.js'
import { ciWatchTool, prFlowTool, releasePreflightTool, releaseDispatchTool } from './tools-native/ci.js'
import { groundTruthTool, businessFunnelTool, opportunityReplayTool, evidenceTool } from './tools-native/business.js'
import { join } from 'node:path'

export const name = 'dsh-phoenix-harness-v3'
export const inject = ['tools']

const MAX_TRACKED = 32

function sessionKey(x) {
  const s = String(x ?? 'unknown')
  return s === '' ? 'unknown' : s
}

function sessionIdOf(exec) {
  try {
    const id = exec?.agent?.session?.id
    return id === undefined || id === null ? 'default' : String(id)
  } catch {
    return 'default'
  }
}

function shortMessage(err) {
  try {
    const m = err?.message ?? String(err)
    return m.length > 300 ? m.slice(0, 300) : m
  } catch {
    return '(unknown error)'
  }
}

export function apply(ctx, config = {}) {
  const root = String(config.workspaceRoot ?? process.cwd())
  const knowledgeRoot = config.knowledgeRoot
    ? String(config.knowledgeRoot)
    : join(root, 'tools', 'phoenix-harness-v3', 'knowledge')
  const sink = createTelemetrySink(root, config.telemetry ?? {})
  const governor = createGovernor(root, sink, config.governor ?? {})
  const contextServer = createContextServer(root, knowledgeRoot)
  const stormTrackers = new Map()
  const fingerprintTrackers = new Map()
  const lastTierByStep = new Map()
  const lastTierBySession = new Map()

  function tracker(map, sid, factory) {
    let t = map.get(sid)
    if (!t) {
      t = factory()
      map.set(sid, t)
      if (map.size > MAX_TRACKED) map.delete(map.keys().next().value)
    }
    return t
  }

  // ── tier observation + governor pre-step gate ────────────────────────────
  ctx.on('agent/pre-step', async (payload, next) => {
    const decision = await next()
    try {
      const sid = sessionKey(payload?.agent?.session?.id)
      const text = latestUserText(payload?.messages)
      const cls = classify(text)
      const key = `${sid}:${payload?.turn}:${payload?.step}`
      lastTierByStep.set(key, cls.label)
      if (lastTierByStep.size > 200) lastTierByStep.delete(lastTierByStep.keys().next().value)
      lastTierBySession.set(sid, cls.label)
      sink.record(sid, {
        ts: nowIso(), event: 'tier.decision', session: sid,
        turn: payload?.turn, step: payload?.step,
        tier: cls.label, effort: cls.effort, preview: text.slice(0, 100),
      })
      return governor.preStep(sid, payload, decision)
    } catch {
      return decision
    }
  })

  ctx.on('agent/request', async (payload, next) => {
    const resolved = await next()
    try {
      const sid = sessionKey(payload?.agent?.session?.id)
      const key = `${sid}:${payload?.turn}:${payload?.step}`
      if (lastTierByStep.has(key)) lastTierBySession.set(sid, lastTierByStep.get(key))
      else lastTierBySession.set(sid, 'standard')
    } catch { /* observation only */ }
    return resolved
  })

  // ── request telemetry (L-001 contract) ───────────────────────────────────
  ctx.on('llm/stream', (options, next) => {
    const sid = sessionKey(options?.sessionId)
    const rec = {
      ts: nowIso(), event: 'llm.request', session: sid,
      provider: options?.provider ?? null,
      model: options?.model ?? null,
      purpose: options?.purpose ?? null,
      effort: options?.reasoningEffort ?? null,
      tier: lastTierBySession.get(sid) ?? null,
      messages: Array.isArray(options?.messages) ? options.messages.length : null,
      estInputChars: estimateInputChars(options?.messages, options?.system),
      usage: null, finish: null, failure: null,
    }
    return (async function* () {
      let downstream
      try {
        downstream = next() // synchronous, exactly once
      } catch (err) {
        rec.finish = 'error'
        rec.failure = { code: err?.code ?? 'UNKNOWN', message: shortMessage(err) }
        sink.record(sid, rec)
        throw err
      }
      try {
        for await (const chunk of downstream) {
          if (chunk?.type === 'usage' && chunk.usage) {
            rec.usage = {
              input: chunk.usage.inputTokens ?? 0,
              output: chunk.usage.outputTokens ?? 0,
              cacheRead: chunk.usage.cacheReadTokens ?? 0,
              cacheWrite: chunk.usage.cacheWriteTokens ?? 0,
              reasoning: chunk.usage.reasoningTokens ?? 0,
            }
          } else if (chunk?.type === 'finish' && chunk.reason) {
            rec.finish = chunk.reason.kind ?? 'ok'
            if (chunk.reason.failure) {
              rec.failure = {
                code: chunk.reason.failure.code ?? 'UNKNOWN',
                message: shortMessage(chunk.reason.failure),
              }
            }
          }
          yield chunk
        }
      } catch (err) {
        rec.finish = 'error'
        rec.failure = { code: err?.code ?? 'UNKNOWN', message: shortMessage(err) }
        throw err
      } finally {
        try {
          if (downstream && typeof downstream.return === 'function') await downstream.return()
        } catch { /* best-effort */ }
        try { governor.noteUsage(sid, rec.usage) } catch { /* never throws */ }
        sink.record(sid, rec) // never throws
      }
    })()
  })

  // ── failures + storms ────────────────────────────────────────────────────
  ctx.on('agent/request-error', async (payload, next) => {
    try {
      const sid = sessionKey(payload?.agent?.session?.id)
      const turn = payload?.turn
      const step = payload?.step
      const code = payload?.failure?.code ?? 'UNKNOWN'
      sink.record(sid, {
        ts: nowIso(), event: 'llm.failure', session: sid, turn, step, code,
        provider: payload?.provider ?? null,
        message: shortMessage(payload?.failure),
      })
      const trk = tracker(stormTrackers, sid, () => createStormTracker())
      const hit = trk.noteFailure(turn, step, code)
      if (hit.event) {
        sink.record(sid, {
          ts: nowIso(), event: hit.event, session: sid,
          turn: hit.turn, step: hit.step, failures: hit.failures, codes: hit.codes,
        })
      }
    } catch { /* never break recovery */ }
    return next()
  })

  // ── loop detection + governor tool classification ────────────────────────
  ctx.on('tools/result', (exec, result) => {
    try {
      const sid = sessionKey(exec?.agent?.session?.id)
      const toolName = exec?.name ?? '(unknown)'
      const chars = resultContentChars(result)
      const { fp, preview } = fingerprintArgs(exec?.arguments)
      sink.record(sid, {
        ts: nowIso(), event: 'tool.result', session: sid, tool: toolName, chars,
        fp, preview,
      })
      governor.noteToolResult(sid, toolName)
      const trk = tracker(fingerprintTrackers, sid, () => createFingerprintTracker())
      const hit = trk.note(toolName, exec?.arguments, chars)
      if (hit.event) {
        sink.record(sid, {
          ts: nowIso(), event: hit.event, session: sid,
          repeat: hit.repeat, tool: hit.name, preview: hit.preview,
        })
      }
    } catch { /* never break the tool pipeline */ }
  })

  // ── tool registration helper ─────────────────────────────────────────────
  function registerTool({ name, description, parameters, execute, presentTitle }) {
    try {
      ctx.tools.register({
        name,
        description,
        parameters: compileParameterSchema(parameters ?? {}),
        output: {
          schema: { type: 'string' },
          render: (_args, value) => [{ type: 'text', text: value }],
        },
        execute: async (args, exec) => {
          try {
            const out = await execute(args, exec)
            return typeof out === 'string' ? out : JSON.stringify(out)
          } catch (err) {
            return `error: tool failed (fail-closed): ${shortMessage(err)}`
          }
        },
        presentCall: (args) => ({ card: 'generic', title: presentTitle ? presentTitle(args) : name, kind: 'other', rawInput: args }),
      })
      return true
    } catch (err) {
      sink.record('plugin', { ts: nowIso(), event: 'tool.register.error', tool: name, message: shortMessage(err) })
      return false
    }
  }

  // ── core Phoenix OS tools ────────────────────────────────────────────────
  registerTool({
    name: 'phoenix_context',
    description:
      'Layered Phoenix context retrieval (retrieval over replay). Load knowledge artifacts on demand instead of re-reading docs: list everything available, load one artifact (knowledge/kernel.md, knowledge/business-twin.md, domain packs under domains/, maps/registries), invariants (the invariant registry), search across all artifacts, or budget (current context pressure vs V3 targets). Serves .phoenix-harness/ + tools/phoenix-harness-v3/knowledge/ only.',
    parameters: {
      action: { type: 'string', required: true, description: 'list | load | invariants | search | budget' },
      file: { type: 'string', description: 'artifact path (load): e.g. knowledge/kernel.md, domains/live-execution.md, domain-map.json' },
      query: { type: 'string', description: 'search term (search action)' },
    },
    execute: async (args) => {
      switch (args.action) {
        case 'list': {
          const all = contextServer.listAll()
          return [
            'PHOENIX CONTEXT (layers A/C/F + knowledge):',
            'HARNESS MAPS:',
            ...all.harness.map((a) => `- ${a}`),
            '',
            'KNOWLEDGE SYSTEM (V3):',
            ...all.knowledge.map((k) => `- knowledge/${k}`),
            '',
            'Use action=load file=<path>. Budgets: phoenix_budget. Mission: phoenix_mission.',
          ].join('\n')
        }
        case 'load': {
          const file = String(args.file ?? '')
          if (!file) return 'error: file= required'
          const loaded = contextServer.loadFile(file)
          if (loaded.error) return `error: ${loaded.error}`
          const note = loaded.truncated ? `\n\n(truncated: ${loaded.truncated} more chars — read the file directly with offset/limit)` : ''
          return `${loaded.name}:\n\n${loaded.text}${note}`
        }
        case 'invariants': {
          const loaded = contextServer.loadFile('invariants.json')
          if (loaded.error) return `error: ${loaded.error}`
          try {
            const inv = JSON.parse(loaded.text)
            return (inv.invariants ?? []).map((i) => `[${i.id}] (${i.severity}) ${i.statement}`).join('\n')
          } catch {
            return loaded.text.slice(0, 6000)
          }
        }
        case 'search': {
          const hits = contextServer.searchArtifacts(args.query)
          if (!hits.length) return `no matches for "${args.query}"`
          return hits.map((h) => `${h.name}:${h.line}: ${h.text}`).join('\n')
        }
        case 'budget': {
          const view = contextServer.pressureView(sink.readAll('*').slice(-2000))
          return [
            `CONTEXT PRESSURE — ${view.verdict}`,
            view.estContextChars !== null ? `est context: ${view.estContextChars} chars (recent max ${view.recentMax})` : 'est context: (no telemetry yet)',
            `targets: normal 30-70K | p95<=96K | hard<=160K | pressure band 96-120K | retain tail 32K | compaction summary 4-6K`,
            `advice: ${view.advice}`,
          ].join('\n')
        }
        default:
          return 'error: action must be list | load | invariants | search | budget'
      }
    },
    presentTitle: (a) => `phoenix_context ${a.action}`,
  })

  registerTool({
    name: 'phoenix_mission',
    description:
      'Compile and manage the typed MissionSpec (the single mission source — never repeated in prompts). create: compile objective/domains/risk/budgets/acceptance/evidence into a durable spec; get: current spec; update: change fields or mark a phase boundary (phase=...); owner_approval: record current-session owner approval (scope required; only valid for riskTier=prod_mutation); close: archive. Budgets are enforced by the governor; prod_mutation dispatch requires owner_approval.',
    parameters: {
      action: { type: 'string', required: true, description: 'create | get | update | owner_approval | close' },
      objective: { type: 'string', description: 'mission objective (create)' },
      businessObjective: { type: 'string', description: 'business objective (create)' },
      technicalObjective: { type: 'string', description: 'technical objective (create)' },
      domains: { type: 'array', items: { type: 'string' }, description: 'domain ids (create/update)' },
      riskTier: { type: 'string', description: 'local_only | prod_readonly | prod_mutation' },
      acceptanceCriteria: { type: 'array', items: { type: 'string' }, description: 'acceptance criteria (max 8)' },
      evidenceRequirements: { type: 'array', items: { type: 'string' }, description: 'evidence requirements (max 8)' },
      tokenBudget: { type: 'integer', description: 'billed-equivalent token budget' },
      modelCallBudget: { type: 'integer', description: 'model-call budget' },
      elapsedBudgetMinutes: { type: 'integer', description: 'elapsed-time budget in minutes' },
      hardStops: { type: 'array', items: { type: 'string' }, description: 'budget_breach | safety_breach | evidence_missing | staleness | uncertain_submission' },
      phase: { type: 'string', description: 'phase name to mark (update)' },
      scope: { type: 'string', description: 'approval scope description (owner_approval)' },
      ack: { type: 'string', description: 'owner_approval requires ack="OWNER" (the operator typed this in the current session)' },
    },
    execute: async (args, exec) => {
      const sid = sessionIdOf(exec)
      if (args.action === 'create') {
        const compiled = compileMission(args)
        if (compiled.error) return `error: ${compiled.error}`
        const written = writeMission(root, sid, compiled.spec)
        if (!written.ok) return `error: ${written.error}`
        return `MISSION CREATED (${written.path.replace(/\\/g, '/')})\n\n${renderMission(compiled.spec)}`
      }
      if (args.action === 'get') {
        const cur = readMission(root, sid)
        if (!cur.exists) return 'no mission yet — phoenix_mission action=create'
        return renderMission(cur.spec)
      }
      if (args.action === 'update') {
        const cur = readMission(root, sid)
        if (!cur.exists) return 'error: no mission to update'
        const spec = cur.spec
        const patch = {}
        for (const k of ['objective', 'businessObjective', 'technicalObjective', 'riskTier', 'authority']) {
          if (args[k] !== undefined && args[k] !== null) patch[k] = String(args[k])
        }
        for (const k of ['domains', 'acceptanceCriteria', 'evidenceRequirements', 'hardStops']) {
          if (args[k] !== undefined) patch[k] = Array.isArray(args[k]) ? args[k].map(String) : []
        }
        for (const [src, dst] of [['tokenBudget', 'tokenBudget'], ['modelCallBudget', 'modelCallBudget'], ['elapsedBudgetMinutes', 'elapsedBudgetMinutes']]) {
          if (args[src] !== undefined) patch[dst] = Number(args[src])
        }
        const compiled = compileMission({ ...spec, ...patch })
        if (compiled.error) return `error: ${compiled.error}`
        compiled.spec.createdAt = spec.createdAt
        compiled.spec.ownerApproval = spec.ownerApproval
        compiled.spec.phases = spec.phases
        compiled.spec.updatedAt = new Date().toISOString()
        if (args.phase) {
          compiled.spec.phases = [...(compiled.spec.phases ?? []), { name: String(args.phase).slice(0, 40), at: compiled.spec.updatedAt }].slice(-12)
        }
        const written = writeMission(root, sid, compiled.spec)
        if (!written.ok) return `error: ${written.error}`
        return `MISSION UPDATED${args.phase ? ` (phase boundary: ${args.phase})` : ''}\n\n${renderMission(compiled.spec)}`
      }
      if (args.action === 'owner_approval') {
        const cur = readMission(root, sid)
        if (!cur.exists) return 'error: no mission exists'
        if (cur.spec.riskTier !== 'prod_mutation') return 'error: owner approval only valid for riskTier=prod_mutation'
        if (String(args.ack ?? '') !== 'OWNER') return 'error: owner_approval requires ack="OWNER" typed by the owner in this current session'
        const scope = String(args.scope ?? '').trim()
        if (!scope || scope.length > 300) return 'error: scope description required (max 300 chars)'
        cur.spec.ownerApproval = { approvedAt: new Date().toISOString(), by: 'owner', scope }
        cur.spec.updatedAt = cur.spec.ownerApproval.approvedAt
        const written = writeMission(root, sid, cur.spec)
        if (!written.ok) return `error: ${written.error}`
        return `OWNER APPROVAL RECORDED (${cur.spec.ownerApproval.approvedAt}) scope: ${scope}\n\n${renderMission(cur.spec)}`
      }
      if (args.action === 'close') {
        const cur = readMission(root, sid)
        if (!cur.exists) return 'error: no mission exists'
        cur.spec.closedAt = new Date().toISOString()
        writeMission(root, sid, cur.spec)
        return `mission closed at ${cur.spec.closedAt}`
      }
      return 'error: action must be create | get | update | owner_approval | close'
    },
    presentTitle: (a) => `phoenix_mission ${a.action}`,
  })

  registerTool({
    name: 'phoenix_checkpoint',
    description:
      'Maintain the Phoenix durable checkpoint (Layer F) for this session: a compact structured progress record (objective, phase, known, unknown, hypotheses, decisions, files changed, tests run, blockers, next action) stored under .phoenix-harness/checkpoints/. Use it to persist engineering state across long work instead of relying on the transcript; get merges nothing, update merges lists (dedup, capped) and replaces scalars.',
    parameters: {
      action: { type: 'string', required: true, description: 'get | update' },
      update: {
        type: 'object', additionalProperties: false,
        description: 'Sparse update: objective, nextAction, phase (strings); known, unknown, hypotheses, decisions, filesChanged, testsRun, blockers (string arrays); replace (bool) to rebuild lists.',
        properties: {
          objective: { type: 'string' },
          nextAction: { type: 'string' },
          phase: { type: 'string' },
          known: { type: 'array', items: { type: 'string' } },
          unknown: { type: 'array', items: { type: 'string' } },
          hypotheses: { type: 'array', items: { type: 'string' } },
          decisions: { type: 'array', items: { type: 'string' } },
          filesChanged: { type: 'array', items: { type: 'string' } },
          testsRun: { type: 'array', items: { type: 'string' } },
          blockers: { type: 'array', items: { type: 'string' } },
          replace: { type: 'boolean' },
        },
      },
    },
    execute: async (args, exec) => {
      const sid = sessionIdOf(exec)
      const current = readCheckpoint(root, sid)
      if (args.action === 'get') {
        const head = current.exists ? 'CURRENT CHECKPOINT' : `NO CHECKPOINT YET (session ${sid}) — created on first update`
        return `${head}\n\n${renderCheckpoint(current)}`
      }
      if (args.action === 'update') {
        const merged = mergeCheckpoint(current, args.update ?? {})
        const written = writeCheckpoint(root, sid, merged)
        if (!written.ok) return `error: ${written.error}`
        return `checkpoint updated (${written.path.replace(/\\/g, '/')}, ${written.chars} chars)\n\n${renderCheckpoint({ data: merged })}`
      }
      return 'error: action must be get | update'
    },
    presentTitle: (a) => `phoenix_checkpoint ${a.action}`,
  })

  registerTool({
    name: 'phoenix_budget',
    description:
      'Round/budget governor view: measured tokens (billed-equivalent), model calls, elapsed time vs the MissionSpec budgets; active waits; bookkeeping streak; warnings and hard-stop verdict. Check at phase boundaries instead of estimating cost.',
    parameters: {},
    execute: async (_args, exec) => {
      const sid = sessionIdOf(exec)
      const view = governor.budgetView(sid)
      return JSON.stringify(view, null, 2)
    },
    presentTitle: () => 'phoenix_budget',
  })

  registerTool({
    name: 'phoenix_telemetry',
    description:
      'Read Phoenix Harness telemetry for this session: model requests, token usage (input/output/cache-read/cache-write/reasoning), failures by code, tiers, storm/loop/governor records, tool-result volume, repeated fingerprints. Use it to explain token/cost behavior instead of estimating.',
    parameters: { scope: { type: 'string', description: 'current (default) | all' } },
    execute: async (args, exec) => {
      const sid = args.scope === 'all' ? '*' : sessionIdOf(exec)
      const records = sink.readAll(sid)
      const reqs = records.filter((r) => r.event === 'llm.request')
      const tools = records.filter((r) => r.event === 'tool.result')
      const fails = records.filter((r) => r.event === 'llm.failure')
      const gov = records.filter((r) => r.event?.startsWith('governor.'))
      const loops = records.filter((r) => r.event === 'tool.repeat' || r.event === 'loop.warning')
      const tiers = {}
      for (const r of records.filter((x) => x.event === 'tier.decision')) tiers[r.tier] = (tiers[r.tier] ?? 0) + 1
      const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0 }
      for (const r of reqs) {
        for (const k of Object.keys(usage)) usage[k] += r.usage?.[k] ?? 0
      }
      const toolChars = tools.reduce((a, t) => a + (t.chars ?? 0), 0)
      const lines = [
        `PHOENIX TELEMETRY — ${args.scope === 'all' ? 'all sessions' : sid}`,
        `requests=${reqs.length} failures=${fails.length} toolResults=${tools.length} toolChars=${toolChars}`,
        `tokens: input=${usage.input} output=${usage.output} cacheRead=${usage.cacheRead} cacheWrite=${usage.cacheWrite} reasoning=${usage.reasoning}`,
        `billedEq=${Math.round(usage.input + 0.1 * usage.cacheRead + usage.output)} (cache-hit @0.1x documented assumption)`,
        `tiers: ${JSON.stringify(tiers)}`,
        `governor: ${gov.length} records | loop: ${loops.length} records`,
        ...gov.slice(-5).map((g) => `  ${g.event} ${g.reason ?? ''} ratio=${g.ratio ?? '-'} streak=${g.streak ?? '-'}`),
        ...loops.slice(-5).map((l) => `  ${l.event} x${l.repeat} ${l.tool} ${(l.preview ?? '').slice(0, 80)}`),
      ]
      return lines.join('\n')
    },
    presentTitle: () => 'phoenix_telemetry',
  })

  // ── native tools (fail-closed, compact, artifact-backed) ────────────────
  const natives = [
    repoSnapshotTool(root),
    symbolTool(root),
    testTool(root),
    remoteTool(),
    productionSnapshotTool(),
    sqlReadonlyTool(),
    releaseVerifyTool(),
    ciWatchTool(governor, root),
    prFlowTool(root),
    releasePreflightTool(root),
    releaseDispatchTool(root, { readMission }),
    groundTruthTool(root),
    businessFunnelTool(root, knowledgeRoot),
    opportunityReplayTool(root),
    evidenceTool(root),
  ]
  const nativeParams = (t) => (t.parameters ?? {})
  for (const t of natives) {
    registerTool({
      name: t.name,
      description: t.description,
      parameters: nativeParams(t),
      execute: (args, exec) => t.execute(args, exec),
      presentTitle: () => t.name,
    })
  }
}
