/**
 * Business-twin tests: the twin answers every required question with
 * evidence; profit labels are never mixed; every figure carries as-of
 * dating; ground-truth/replay tools preserve source labels.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, existsSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const REPO = resolve(ROOT, '..', '..')
const TWIN = join(ROOT, 'knowledge', 'business-twin.md')

const REQUIRED_QUESTIONS = [
  /why revenue is zero/i, /no.?alpha/i, /addressable/i, /engineering.?day|EV per/i,
  /funnel/i, /asset|route.*size/i, /expected.*conservative.*realized/i,
  /competitor|missed/i, /next.*move|highest/i,
]

test('business-twin.md exists, is bounded, and answers all 9 questions', () => {
  assert.ok(existsSync(TWIN), 'business-twin.md missing')
  const text = readFileSync(TWIN, 'utf8')
  assert.ok(text.length < 30000, 'business twin too large')
  for (const q of REQUIRED_QUESTIONS) {
    assert.ok(q.test(text), `question not answered: ${q}`)
  }
  assert.ok(/EVIDENCE SOURCES/.test(text), 'sources section required')
})

test('business twin separates FACT/HYPOTHESIS/PROPOSAL/UNKNOWN and dates figures', () => {
  const text = readFileSync(TWIN, 'utf8')
  assert.ok(/FACT/.test(text) && /HYPOTHESIS|PROPOSAL/.test(text))
  // figures are date-stamped: check at least several "2026-" dates present
  const dates = text.match(/2026-\d{2}-\d{2}/g) ?? []
  assert.ok(dates.length >= 5, `expected dated figures, found ${dates.length} dates`)
})

test('business twin never calls expected/shadow PnL realized revenue', () => {
  const text = readFileSync(TWIN, 'utf8')
  // label-mixing patterns: expected/shadow described AS realized (booked, counted,
  // reported, treated) — negated safety statements ("never realized") are fine.
  const sentences = text.split(/(?<=\.)\s+/)
  const mixing = sentences.filter((s) =>
    /\b(expected|shadow)\b.{0,120}\b(is|was|are|counts? as|counted|treated|booked|reported)\b.{0,60}\brealized\b/i.test(s)
    && !/\b(never|not|no)\b.{0,60}\brealized\b/i.test(s))
  assert.deepEqual(mixing, [], `label mixing found: ${JSON.stringify(mixing.map((s) => s.slice(0, 140)), null, 2)}`)
})

test('opportunity replay preserves source labels (never converts fixture PnL to realized)', async () => {
  const { pathToFileURL } = await import('node:url')
  const { opportunityReplayTool } = await import(pathToFileURL(join(ROOT, 'src', 'tools-native', 'business.js')).href)
  const tool = opportunityReplayTool(REPO)
  const out = await tool.execute({ source: 'all' })
  assert.ok(/OPPORTUNITY REPLAY/.test(out))
  assert.ok(/conservativeNetPnl/.test(out))
  assert.ok(/NOT APPLICABLE \(fixture replay/.test(out), 'fixture PnL must not be labeled realized')
})

test('ground truth ledger tool indexes real ledgers and stamps sources', async () => {
  const { pathToFileURL } = await import('node:url')
  const { groundTruthTool } = await import(pathToFileURL(join(ROOT, 'src', 'tools-native', 'business.js')).href)
  const tool = groundTruthTool(REPO)
  const list = await tool.execute({ ledger: 'list' })
  assert.ok(/platform/.test(list) && /a1/.test(list) && /missing_alpha/.test(list))
  const platform = await tool.execute({ ledger: 'platform' })
  assert.ok(/ECONOMIC_LEDGER/.test(platform))
  assert.ok(/realizedNetPnl/.test(platform))
  const missing = await tool.execute({ ledger: 'bogus' })
  assert.ok(/unknown ledger/.test(missing))
})
