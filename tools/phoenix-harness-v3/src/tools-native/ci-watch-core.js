/**
 * CI-watch terminal semantics core (V3 reliability hardening, 2026-08-24).
 *
 * Contract: a CI watcher waits for a TERMINAL outcome, not merely any state
 * transition. `queued -> in_progress` is NOT a wake reason. An already-
 * terminal target returns IMMEDIATELY after one fresh read, so repeated
 * calls for the same finished run are cheap and never re-enter a long wait.
 *
 * Terminal predicate is deliberately GENERIC (no fragile whitelist):
 *   status === "completed" AND conclusion is a real value
 * which covers completed/success, completed/failure, completed/cancelled,
 * completed/skipped, completed/timed_out, completed/action_required,
 * completed/neutral, completed/stale.
 *
 * Pure functions + one small driver (`watchCi`) so the native tool stays a
 * thin shell and every semantic is unit-testable without GitHub.
 */

/** A conclusion is "real" when it is a concrete terminal value. */
export function isRealConclusion(conclusion) {
  const c = String(conclusion ?? '').trim().toLowerCase()
  return c !== '' && c !== 'pending' && c !== 'null' && c !== 'undefined' && c !== '-' && c !== 'none'
}

/** Generic terminal predicate: completed status + real conclusion. */
export function isRunTerminal(status, conclusion) {
  return String(status ?? '').trim().toLowerCase() === 'completed' && isRealConclusion(conclusion)
}

/** Parse one gh --jq '.status + "|" + (.conclusion // "pending")' line. */
export function parseRunState(raw) {
  const s = String(raw ?? '').trim()
  const idx = s.indexOf('|')
  if (idx < 0) return { status: s, conclusion: '' }
  return { status: s.slice(0, idx).trim(), conclusion: s.slice(idx + 1).trim() }
}

/** Parse the ';'-joined per-check lines returned for a SHA watch. */
export function parseCheckStates(raw) {
  return String(raw ?? '')
    .split(';')
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map(parseRunState)
}

/**
 * Counts for a SHA summary. `pending` counts every non-terminal check
 * (queued/in_progress/waiting/…); anything terminal but outside the named
 * conclusions lands in `other`. A partially-running SHA is never "final".
 */
export function summarizeChecks(states) {
  const counts = { total: states.length, success: 0, failure: 0, cancelled: 0, skipped: 0, pending: 0, other: 0 }
  for (const st of states) {
    if (!isRunTerminal(st.status, st.conclusion)) { counts.pending += 1; continue }
    const c = st.conclusion.toLowerCase()
    if (c === 'success') counts.success += 1
    else if (c === 'failure' || c === 'timed_out' || c === 'startup_failure') counts.failure += 1
    else if (c === 'cancelled') counts.cancelled += 1
    else if (c === 'skipped' || c === 'stale') counts.skipped += 1
    else counts.other += 1
  }
  return counts
}

/** All discovered checks terminal — requires at least one check to exist. */
export function allChecksTerminal(states) {
  return states.length > 0 && states.every((st) => isRunTerminal(st.status, st.conclusion))
}

/**
 * Watch driver shared by runId and sha modes.
 * @param {object} p
 *   read        async () => raw state string (throws = read error)
 *   mode        'run' (single Actions run) | 'sha' (all check runs of a SHA)
 *   intervalMs  poll interval inside the native tool (default 10_000)
 *   maxWaitMs   bounded deadline (fail-closed)
 *   signal      optional AbortSignal (prompt exit, no extra read after abort)
 * @returns {Promise<{ok:true,state:string,immediate:boolean,waitedMs:number,checks:number}
 *                   |{ok:false,error:string,waitedMs:number,checks:number,aborted?:boolean,lastState?:string}>}
 */
export async function watchCi(p) {
  const mode = p.mode === 'sha' ? 'sha' : 'run'
  const intervalMs = Math.min(Math.max(Number(p.intervalMs ?? 10000) || 10000, 500), 120000)
  const done = (raw) => {
    const states = mode === 'run' ? [parseRunState(raw)] : parseCheckStates(raw)
    return mode === 'run' ? isRunTerminal(states[0].status, states[0].conclusion) : allChecksTerminal(states)
  }
  let initial
  try {
    initial = await p.read()
  } catch (err) {
    return { ok: false, error: `cannot read CI state: ${String(err?.message ?? err)}`, waitedMs: 0, checks: 1 }
  }
  // Re-entry safety: already-terminal -> immediate return, zero waiting.
  if (done(initial)) return { ok: true, state: initial, immediate: true, waitedMs: 0, checks: 1 }

  const { waitForState } = await import('../wait.js')
  let lastState = initial
  const result = await waitForState(async () => {
    const raw = await p.read()
    lastState = raw
    return done(raw) ? raw : null
  }, {
    intervalMs,
    maxWaitMs: p.maxWaitMs,
    signal: p.signal,
    failClosedMessage: p.failClosedMessage ?? 'CI watch deadline reached before a terminal outcome (fail closed)',
  })
  if (result.ok) return { ok: true, state: result.state, immediate: false, waitedMs: result.waitedMs, checks: result.checks }
  return {
    ok: false,
    error: result.error,
    waitedMs: result.waitedMs,
    checks: result.checks,
    ...(result.aborted ? { aborted: true } : {}),
    lastState,
  }
}

/** Model-facing compact terminal block for runId mode. */
export function formatRunTerminal(runId, parsed, waitedSeconds, checks) {
  return [
    'CI_WATCH_TERMINAL',
    `run_id=${runId}`,
    `status=${parsed.status}`,
    `conclusion=${parsed.conclusion}`,
    `waited_seconds=${waitedSeconds}`,
    `checks=${checks}`,
  ].join('\n')
}

/** Model-facing compact terminal block for sha mode (never says "final" while any check runs). */
export function formatShaTerminal(sha, states, waitedSeconds, checks) {
  const c = summarizeChecks(states)
  return [
    'CI_WATCH_TERMINAL',
    `sha=${sha}`,
    `checks_total=${c.total}`,
    `success=${c.success}`,
    `failure=${c.failure}`,
    `cancelled=${c.cancelled}`,
    `skipped=${c.skipped}`,
    `pending=${c.pending}`,
    c.other > 0 ? `other=${c.other}` : null,
    `waited_seconds=${waitedSeconds}`,
    `polls=${checks}`,
  ].filter((l) => l !== null).join('\n')
}
