/**
 * Deterministic wait core (Phase 5/6) — the model-suspension seam.
 * Waits run INSIDE a native tool: while the tool executes, the model is
 * suspended; the tool returns exactly when the watched condition is met (or
 * fails closed at the deadline). No polling rounds are ever generated.
 *
 * Reliability hardening (V3, 2026-08-24):
 *  - optional AbortSignal (`opts.signal`): abortable sleep, prompt exit after
 *    abort, no orphan timer, and NO extra check() call once aborted;
 *  - existing callers stay compatible (signal is optional);
 *  - the bounded deadline remains fail-closed.
 */

/**
 * Poll `check` until it returns a non-null state or the deadline passes.
 * @param {Function} check async () => state|null — null means "condition not met"
 * @param {object} opts {intervalMs, maxWaitMs, failClosedMessage, signal?}
 * @returns {Promise<{ok:true,state,waitedMs,checks}|{ok:false,error,waitedMs,checks,aborted?:boolean}>}
 */
export async function waitForState(check, opts = {}) {
  const intervalMs = opts.intervalMs ?? 30000
  const maxWaitMs = Math.min(opts.maxWaitMs ?? 30 * 60 * 1000, 120 * 60 * 1000)
  const signal = opts.signal ?? null
  const started = Date.now()
  let checks = 0

  const abortedResult = () => ({
    ok: false,
    error: 'wait aborted',
    waitedMs: Date.now() - started,
    checks,
    aborted: true,
  })

  for (;;) {
    // Prompt exit on abort — and never an extra check() after abort.
    if (signal?.aborted) return abortedResult()
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
    const remaining = maxWaitMs - (Date.now() - started)
    if (remaining <= 0) continue
    const outcome = await abortableSleep(Math.min(intervalMs, remaining), signal)
    // Loop head re-checks `signal?.aborted` first, so an abort during sleep
    // exits promptly without touching check() again; the timer is cleared,
    // leaving no orphan handle.
    if (outcome === 'aborted') return abortedResult()
  }
}

/**
 * Bounded sleep that wakes early on abort. Resolves 'sleep' when the full
 * interval elapsed, 'aborted' when the signal fired. The setTimeout handle
 * is always cleared (no orphan timer).
 */
function abortableSleep(ms, signal) {
  if (!signal) return new Promise((resolve) => setTimeout(() => resolve('sleep'), ms))
  if (signal.aborted) return Promise.resolve('aborted')
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      try { signal.removeEventListener('abort', onAbort) } catch { /* ignore */ }
      resolve('sleep')
    }, ms)
    function onAbort() {
      clearTimeout(timer)
      resolve('aborted')
    }
    try { signal.addEventListener('abort', onAbort, { once: true }) } catch { /* ignore */ }
  })
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export { sleep }
