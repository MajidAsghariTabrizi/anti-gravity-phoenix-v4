/**
 * Reasoning-tier classification (observe-only — V2 validated decision).
 * The agent-scoped model-selection owns effort; a preset-level effort
 * mutation is a silent-downgrade risk and is never performed. Tiers are
 * evidence for routing decisions, recorded in telemetry.
 */

const CRITICAL_PATTERNS = [
  /\b(production|prod)\b/i, /\brelease\b/i, /\bdeploy/i, /\bmutation\b/i,
  /\blive[-_ ]execution\b/i, /\bmoney[-_ ]path\b/i, /\bowner\b/i,
  /\barm(ed|ing)?\b/i, /\bunpause\b/i, /\bsubmit\b/i, /\bsigner\b/i,
  /\bnonce\b/i, /\breconciliation\b/i, /\brollback\b/i, /\bactivate\b/i,
  /\bfail[- ]close\b/i, /\bsecurity\b/i, /\bsecret\b/i, /\bgate\b/i,
  /\bsafety\b/i, /\badversarial\b/i, /\bfinancial\b/i, /\bprofit\b/i,
]
const MECHANICAL_PATTERNS = [
  /^(hi|hello|hey|ok|thanks|continue|resume|next)\b/i,
  /\bformat(ting)?\b/i, /\brename\b/i, /\blint\b/i, /\bpretty[- ]?print\b/i,
]

export function classify(text) {
  if (!text || String(text).trim() === '') return { label: 'unknown', effort: null }
  if (CRITICAL_PATTERNS.some((p) => p.test(text))) return { label: 'critical', effort: 'max' }
  if (MECHANICAL_PATTERNS.some((p) => p.test(text)) && String(text).length < 400) {
    return { label: 'mechanical', effort: 'low' }
  }
  return { label: 'standard', effort: 'default' }
}

export function latestUserText(messages) {
  if (!Array.isArray(messages)) return ''
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    const role = m?.role ?? m?.message?.role
    if (role === 'user') {
      const content = m?.content ?? m?.message?.content
      if (typeof content === 'string') return content
      if (Array.isArray(content)) {
        const parts = content.filter((c) => typeof c === 'string' || c?.type === 'text')
          .map((c) => (typeof c === 'string' ? c : c.text ?? ''))
        return parts.join('\n')
      }
      return ''
    }
  }
  return ''
}

export function estimateInputChars(messages, system) {
  let n = typeof system === 'string' ? system.length : 0
  if (Array.isArray(messages)) {
    for (const m of messages) {
      const content = m?.content ?? m?.message?.content ?? ''
      n += typeof content === 'string' ? content.length : (Array.isArray(content) ? JSON.stringify(content).length : 0)
    }
  }
  return n
}

/** Goal-round source detection: goal driver messages carry source.kind === 'goal'. */
export function isGoalRoundMessage(m) {
  return m?.source?.kind === 'goal' || m?.message?.source?.kind === 'goal'
}

export function isGoalRoundStep(messages) {
  if (!Array.isArray(messages)) return false
  return messages.some(isGoalRoundMessage)
}
