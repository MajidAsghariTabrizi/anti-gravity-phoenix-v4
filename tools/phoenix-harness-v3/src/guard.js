/**
 * Storm + loop detection (observational; V3 adds argument fingerprints).
 * Never blocks legitimate work; every tracker is bounded-memory.
 */
import { createHash } from 'node:crypto'

export function createStormTracker() {
  let window = [] // {turn, step, code}
  let lastEvent = null
  return {
    noteFailure(turn, step, code) {
      window.push({ turn, step, code })
      const key = `${turn}/${step}`
      const same = window.filter((w) => `${w.turn}/${w.step}` === key)
      if (window.length > 200) window = window.slice(-200)
      if (same.length >= 4 && lastEvent !== `warn-${key}`) {
        lastEvent = `warn-${key}`
        return { event: 'retry.storm.warn', turn, step, failures: same.length, codes: [...new Set(same.map((w) => w.code))] }
      }
      if (same.length >= 8 && lastEvent !== `critical-${key}`) {
        lastEvent = `critical-${key}`
        return { event: 'retry.storm.critical', turn, step, failures: same.length, codes: [...new Set(same.map((w) => w.code))] }
      }
      return { event: null }
    },
  }
}

export function createFingerprintTracker() {
  const recent = new Map() // `${tool}|${fp}` -> count
  const warned = new Map()
  return {
    note(tool, args, chars) {
      const { fp, preview } = (() => {
        try {
          let obj = args
          if (typeof args === 'string') {
            try { obj = JSON.parse(args) } catch { obj = args }
          }
          return { fp: createHash('sha256').update(JSON.stringify(obj ?? '')).digest('hex').slice(0, 16), preview: String(JSON.stringify(obj ?? '')).slice(0, 120) }
        } catch {
          return { fp: '(unfp)', preview: '(unfp)' }
        }
      })()
      const key = `${tool}|${fp}`
      const count = (recent.get(key) ?? 0) + 1
      recent.set(key, count)
      if (recent.size > 300) {
        const first = recent.keys().next().value
        recent.delete(first)
      }
      if (count === 3 && !warned.has(key)) {
        warned.set(key, 1)
        return { event: 'tool.repeat', repeat: count, name: tool, preview }
      }
      if (count === 5 && warned.get(key) === 1) {
        warned.set(key, 2)
        return { event: 'loop.warning', repeat: count, name: tool, preview }
      }
      return { event: null }
    },
  }
}

export function resultContentChars(result) {
  try {
    if (typeof result === 'string') return result.length
    const text = result?.content ?? result?.result ?? result?.output ?? result?.text
    if (typeof text === 'string') return text.length
    if (Array.isArray(text)) {
      return text.reduce((a, t) => a + (typeof t === 'string' ? t.length : (t?.text?.length ?? 0)), 0)
    }
    return 0
  } catch {
    return 0
  }
}
