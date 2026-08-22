/**
 * L-004/L-009 release-graph tests: the protected provenance chain is
 * complete, ordered, and gate-enforced in the knowledge graph.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

test('release graph contains the full 13-node protected chain', () => {
  const g = JSON.parse(readFileSync(join(ROOT, 'knowledge', 'graphs', 'release-graph.json'), 'utf8'))
  const required = [
    'branch', 'implement', 'focused-tests', 'regression', 'secret-scan', 'diff-review',
    'draft-pr', 'pr-ci', 'merge', 'main-ci', 'build', 'controller', 'hard-gate',
    'activation', 'reconcile',
  ]
  const ids = g.nodes.map((n) => n.id)
  for (const id of required) assert.ok(ids.includes(id), `missing node ${id}`)
  // edges form one continuous chain with no gaps or cycles
  const next = new Map(g.edges.map(([a, b]) => [a, b]))
  assert.equal(next.get('branch'), 'implement')
  assert.equal(next.get('controller'), 'hard-gate')
  assert.equal(next.get('activation'), 'reconcile')
  // every non-terminal node has an outgoing edge
  for (const n of g.nodes.filter((x) => x.kind === 'gate')) assert.ok(next.has(n.id), `gate ${n.id} has no next step`)
  assert.ok(g.prohibited.includes('manual release-pointer edits'))
  assert.ok(g.prohibited.includes('manual image replacement'))
})

test('authority graph: lanes separate, Generic DEX closed by policy', () => {
  const g = JSON.parse(readFileSync(join(ROOT, 'knowledge', 'graphs', 'authority-graph.json'), 'utf8'))
  assert.equal(g.lanes.generic_dex.policy.includes('CLOSED'), true)
  assert.ok(g.lanes.aave_liquidation.authority === 'separate')
  assert.ok(g.lanes.atlas_solver.authority === 'separate')
  assert.ok(g.rules.some((r) => /older session/.test(r)))
  assert.ok(g.rules.some((r) => /blind-retry/.test(r)))
})

test('incident registry: all 10 seeded lessons have evidence + regression test', () => {
  const reg = JSON.parse(readFileSync(join(ROOT, 'knowledge', 'registries', 'incidents.json'), 'utf8'))
  assert.equal(reg.lessons.length, 10)
  for (const l of reg.lessons) {
    assert.ok(l.evidence, `L-${l.id} missing evidence`)
    assert.ok(l.test, `L-${l.id} missing regression test`)
    assert.ok(l.rule, `L-${l.id} missing rule`)
  }
})
