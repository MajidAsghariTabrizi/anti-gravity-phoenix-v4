/**
 * Deterministic wait core (Phase 5/6) — the model-suspension seam.
 * Waits run INSIDE a native tool: while the tool executes, the model is
 * suspended; the tool returns exactly when the watched state changes (or
 * fails closed at the deadline). No polling rounds are ever generated.
 */

/**
 * Poll `check` until it returns a non-null state or the deadline passes.
 * @param {Function} check async () => state|null — null means "no change"
 * @param {object} opts {intervalMs, maxWaitMs, failClosedMessage}
 * @returns {Promise<{ok:true,state,waitedMs,checks}|{ok:false,error,waitedMs,checks}>}
 */
export async function waitForState(check, opts = {}) {
  const intervalMs = opts.intervalMs ?? 30000
  const maxWaitMs = Math.min(opts.maxWaitMs ?? 30 * 60 * 1000, 120 * 60 * 1000)
  const started = Date.now()
  let checks = 0
  for (;;) {
    checks += 1
    let state = null
    try {
      state = await check()
    } catch (err) {
      return { ok: false, error: `wait check failed: ${String(err?.message ?? err)}`, waitedMs: Date.now() - started, checks }
    }
    if (state !== null && state !== undefined) {
      return { ok: true, state, waitedMs: Date.now() - started, checks }
    }
    if (Date.now() - started >= maxWaitMs) {
      return { ok: false, error: opts.failClosedMessage ?? 'wait deadline reached with no state change (fail closed)', waitedMs: Date.now() - started, checks }
    }
    await sleep(Math.min(intervalMs, maxWaitMs - (Date.now() - started)))
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
