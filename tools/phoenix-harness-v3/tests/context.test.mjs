/**
 * Context compiler tests: layered retrieval, freshness classes, pressure
 * verdicts, path safety.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { existsSync, readFileSync } from 'node:fs'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const MOD = pathToFileURL(join(ROOT, 'src', 'context.js')).href

const KNOWLEDGE = join(ROOT, 'knowledge')
// Hermetic harness root: CI machines have no curated .phoenix-harness at the
// repo root, so the harness-layer assertions run against a committed fixture.
const FIXTURE_REPO = join(ROOT, 'tests', 'fixtures', 'context-harness')

test('list serves both harness maps and knowledge layers', async () => {
  const { createContextServer } = await import(MOD)
  const server = createContextServer(FIXTURE_REPO, KNOWLEDGE)
  const all = server.listAll()
  assert.ok(all.harness.includes('invariants.json'))
  assert.ok(all.harness.includes('domain-map.json'))
  assert.ok(all.harness.some((f) => f.startsWith('domains/')))
  assert.ok(all.knowledge.includes('knowledge/kernel.md'))
  assert.ok(all.knowledge.includes('knowledge/business-twin.md'))
  assert.ok(all.knowledge.includes('knowledge/freshness-policy.md'))
  assert.ok(all.knowledge.some((f) => f.startsWith('knowledge/ontology/')))
  assert.ok(all.knowledge.some((f) => f.startsWith('knowledge/graphs/')))
  assert.ok(all.knowledge.some((f) => f.startsWith('knowledge/registries/')))
})

test('load returns kernel and domain packs; traversal refused', async () => {
  const { createContextServer } = await import(`${MOD}?t=1`)
  const server = createContextServer(FIXTURE_REPO, KNOWLEDGE)
  const kernel = server.loadFile('knowledge/kernel.md')
  assert.ok(!kernel.error)
  assert.ok(/Source-of-truth order/.test(kernel.text))
  const domain = server.loadFile('domains/live-execution.md')
  assert.ok(!domain.error, domain.error)
  assert.ok(/fail closed/i.test(domain.text))
  assert.ok(server.loadFile('../AGENTS.local.md').error, 'traversal must fail')
  assert.ok(server.loadFile('does-not-exist.md').error)
})

test('search finds matches across both roots', async () => {
  const { createContextServer } = await import(`${MOD}?t=2`)
  const server = createContextServer(FIXTURE_REPO, KNOWLEDGE)
  const hits = server.searchArtifacts('Realized Net PnL')
  assert.ok(hits.length > 0)
  assert.ok(hits.every((h) => h.line > 0 && h.name))
})

test('pressureView classifies context against V3 targets', async () => {
  const { createContextServer } = await import(`${MOD}?t=3`)
  const server = createContextServer(FIXTURE_REPO, KNOWLEDGE)
  const mk = (n, chars) => Array.from({ length: n }, () => ({ event: 'llm.request', estInputChars: chars }))
  assert.equal(server.pressureView([]).verdict, 'NORMAL')
  assert.equal(server.pressureView(mk(10, 40000)).verdict, 'NORMAL')
  assert.equal(server.pressureView(mk(10, 80000)).verdict, 'ELEVATED')
  assert.equal(server.pressureView(mk(10, 110000)).verdict, 'PRESSURE')
  assert.equal(server.pressureView(mk(10, 140000)).verdict, 'LIMIT')
  const v = server.pressureView(mk(10, 140000))
  assert.ok(v.targets.hardLimit === 160000)
  assert.ok(v.targets.retainTail === 32768)
})

test('knowledge kernel + freshness policy files exist and are compact', () => {
  for (const f of ['kernel.md', 'freshness-policy.md']) {
    const p = join(KNOWLEDGE, f)
    assert.ok(existsSync(p), `${f} missing`)
    assert.ok(readFileSync(p, 'utf8').length < 8000, `${f} too large for a served layer`)
  }
  const fp = readFileSync(join(KNOWLEDGE, 'freshness-policy.md'), 'utf8')
  assert.ok(/prod-live.*5 minutes/.test(fp))
  assert.ok(/STALE/.test(fp))
})
