/**
 * Round/budget governor (Phase 6).
 *
 * Enforcement seams (verified against the installed harness):
 *  - agent/pre-step waterfall may return {kind:'reject'} — the turn ends
 *    BLOCKED with ZERO model calls (dsh-agent-loop consumes it).
 *  - llm/stream telemetry gives exact per-request usage for budgets.
 *
 * Policy (narrow, fail-closed):
 *  - REJECT a step when: (a) an active wait is registered and the last
 *    >=2 tool steps were bookkeeping-only (no-op spinning), or
 *    (b) a mission hard stop is breached and the step is a goal-round
 *    continuation. Rejected turns cost zero tokens.
 *  - Waits themselves block INSIDE native tools (wait.js): the model is
 *    suspended and wakes on state change — it never spins rounds.
 *  - Budgets/loops/storms/staleness are tracked and observable via
 *    phoenix_budget + phoenix_telemetry; message fabrication is never
 *    attempted (contract-honest, V2 lesson).
 */
import { readMission } from './mission.js'
import { isGoalRoundStep } from './tier.js'

const BOOKKEEPING_TOOLS = new Set([
  'get_goal', 'update_goal', 'phoenix_checkpoint', 'job_list', 'list_agents', 'phoenix_telemetry',
])
const CACHE_HIT_DISCOUNT = 0.1

export function createGovernor(root, sink, config = {}) {
  const state = {
    sessions: new Map(), // sid -> {budgets, bookkeepingStreak, waits, lastWarn}
    config: {
      noOpRejectStreak: config.noOpRejectStreak ?? 2,
      warnAtRatio: config.warnAtRatio ?? 0.8,
      bookkeepingOnlyTools: config.bookkeepingOnlyTools ?? BOOKKEEPING_TOOLS,
    },
  }

  function session(sid) {
    let s = state.sessions.get(sid)
    if (!s) {
      s = {
        budgets: { tokens: 0, modelCalls: 0, outputTokens: 0, startedAt: Date.now(), elapsedMs: 0, warnings: 0, stops: 0 },
        bookkeepingStreak: 0,
        waits: new Map(), // id -> {deadlineMs, reason}
        lastWarnTurn: -1,
      }
      state.sessions.set(sid, s)
      if (state.sessions.size > 32) state.sessions.delete(state.sessions.keys().next().value)
    }
    return s
  }

  /** llm/stream usage accumulation (called from the stream wrapper). */
  function noteUsage(sid, usage) {
    const s = session(sid)
    if (!usage) return
    s.budgets.tokens += (usage.input ?? 0) + CACHE_HIT_DISCOUNT * (usage.cacheRead ?? 0) + (usage.output ?? 0)
    s.budgets.outputTokens += usage.output ?? 0
    s.budgets.modelCalls += 1
    s.budgets.elapsedMs = Date.now() - s.budgets.startedAt
  }

  /** tools/result classification (called from the tools/result listener). */
  function noteToolResult(sid, toolName) {
    const s = session(sid)
    if (state.config.bookkeepingOnlyTools.has(toolName)) s.bookkeepingStreak += 1
    else s.bookkeepingStreak = 0
    return s.bookkeepingStreak
  }

  /** Register/refresh a wait (called by wait tools). */
  function registerWait(sid, id, deadlineMs, reason) {
    const s = session(sid)
    s.waits.set(id, { deadlineMs, reason })
  }
  function clearWait(sid, id) {
    session(sid).waits.delete(id)
  }
  function activeWaits(sid) {
    const now = Date.now()
    const s = session(sid)
    for (const [id, w] of [...s.waits]) if (now > w.deadlineMs) s.waits.delete(id)
    return [...s.waits.entries()]
  }

  /**
   * agent/pre-step decision (after next() resolves).
   * Returns the decision unchanged, or {kind:'reject'} (turn blocked).
   */
  function preStep(sid, payload, decision) {
    const s = session(sid)
    const waits = activeWaits(sid)
    const mission = readMission(root, sid)

    // (a) no-op spinning during an active wait -> block the turn (zero cost)
    if (waits.length > 0 && s.bookkeepingStreak >= state.config.noOpRejectStreak) {
      sink.record(sid, {
        ts: new Date().toISOString(), event: 'governor.reject', session: sid,
        reason: 'bookkeeping-steps-during-wait', streak: s.bookkeepingStreak,
        waits: waits.map(([id, w]) => ({ id, reason: w.reason })),
      })
      return { kind: 'reject' }
    }

    // (b) mission hard stops on goal-round continuations -> block (zero cost)
    if (mission.exists && mission.spec?.hardStops?.length && isGoalRoundStep(payload?.messages)) {
      const b = s.budgets
      const m = mission.spec
      const breached =
        (m.hardStops.includes('budget_breach') && (b.tokens >= m.budgets.tokens || b.modelCalls >= m.budgets.modelCalls || b.elapsedMs >= m.budgets.elapsedMinutes * 60000)) ||
        (m.hardStops.includes('staleness') && b.bookkeepingStreak >= 4)
      if (breached) {
        s.budgets.stops += 1
        sink.record(sid, {
          ts: new Date().toISOString(), event: 'governor.stop', session: sid,
          reason: 'hard-stop-breached', tokens: b.tokens, modelCalls: b.modelCalls,
          budget: m.budgets, streak: b.bookkeepingStreak,
        })
        return { kind: 'reject' }
      }
    }

    // warnings (observable; never injected into messages)
    if (mission.exists && mission.spec?.budgets) {
      const ratio = Math.max(
        s.budgets.tokens / mission.spec.budgets.tokens,
        s.budgets.modelCalls / mission.spec.budgets.modelCalls,
        s.budgets.elapsedMs / (mission.spec.budgets.elapsedMinutes * 60000),
      )
      const turn = payload?.turn ?? -1
      if (ratio >= state.config.warnAtRatio && turn !== s.lastWarnTurn) {
        s.lastWarnTurn = turn
        s.budgets.warnings += 1
        sink.record(sid, {
          ts: new Date().toISOString(), event: 'governor.warn', session: sid,
          ratio: Number(ratio.toFixed(2)), tokens: s.budgets.tokens,
          modelCalls: s.budgets.modelCalls, elapsedMinutes: Math.round(s.budgets.elapsedMs / 60000),
        })
      }
    }
    return decision
  }

  /** Budget snapshot for phoenix_budget. */
  function budgetView(sid) {
    const s = session(sid)
    const mission = readMission(root, sid)
    const b = s.budgets
    const out = {
      session: sid,
      measured: {
        tokensBilledEq: Math.round(b.tokens),
        modelCalls: b.modelCalls,
        outputTokens: b.outputTokens,
        elapsedMinutes: Number((b.elapsedMs / 60000).toFixed(1)),
      },
      waits: activeWaits(sid).map(([id, w]) => ({ id, reason: w.reason, deadlineMs: w.deadlineMs })),
      bookkeepingStreak: b.bookkeepingStreak,
      warnings: b.warnings,
      stops: b.stops,
    }
    if (mission.exists && mission.spec?.budgets) {
      const m = mission.spec.budgets
      out.budget = m
      out.ratios = {
        tokens: Number((b.tokens / m.tokens).toFixed(2)),
        modelCalls: Number((b.modelCalls / m.modelCalls).toFixed(2)),
        elapsed: Number((b.elapsedMs / (m.elapsedMinutes * 60000)).toFixed(2)),
      }
      out.verdict = Math.max(...Object.values(out.ratios)) >= 1 ? 'HARD-STOP' : (Math.max(...Object.values(out.ratios)) >= state.config.warnAtRatio ? 'WARN' : 'OK')
    }
    return out
  }

  return { noteUsage, noteToolResult, preStep, registerWait, clearWait, activeWaits, budgetView }
}
