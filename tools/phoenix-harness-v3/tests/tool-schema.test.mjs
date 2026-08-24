/**
 * L-002 regression: every registered tool parameter schema must be an
 * object-root JSON Schema at the provider boundary. Drives the real
 * plugin and walks every phoenix_* tool schema for violations; also
 * unit-tests compileParameterSchema edge cases.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, rmSync, mkdirSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

/** Mark a temp dir as a Phoenix repo (resolver requires both markers). */
function markRepo(tmp) {
  mkdirSync(join(tmp, '.git'), { recursive: true })
  mkdirSync(join(tmp, '.phoenix-harness'), { recursive: true })
}

function walkProblems(params, prefix = '') {
  const problems = []
  if (!params || typeof params !== 'object') return ['parameters missing']
  if (params.type !== 'object') problems.push(`${prefix}: root type ${JSON.stringify(params.type)} must be object`)
  if (!params.properties || typeof params.properties !== 'object') problems.push(`${prefix}: properties missing`)
  if (params.required !== undefined && !Array.isArray(params.required)) problems.push(`${prefix}: required not array`)
  const walk = (node, p) => {
    if (Array.isArray(node)) { node.forEach((v, i) => walk(v, `${p}[${i}]`)); return }
    if (node !== null && typeof node === 'object') {
      for (const [k, v] of Object.entries(node)) {
        if (k === 'type' && (v === null || v === undefined)) problems.push(`${p}.type is null`)
        walk(v, p ? `${p}.${k}` : k)
      }
    }
  }
  walk(params, prefix)
  return problems
}

test('every phoenix_* tool schema is object-root and provider-safe', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-schema-'))
  try {
    markRepo(tmp)
    const { apply } = await import(pathToFileURL(join(ROOT, 'src', 'plugin.js')).href)
    const definitions = []
    const ctx = { on() {}, get() { return undefined }, tools: { register(def) { definitions.push(def) } } }
    apply(ctx, { workspaceRoot: tmp })
    const phoenixTools = definitions.filter((d) => d.name.startsWith('phoenix_'))
    assert.ok(phoenixTools.length >= 19, `expected >=19 phoenix tools, got ${phoenixTools.length}`)
    for (const t of phoenixTools) {
      const problems = walkProblems(t.parameters, t.name)
      assert.deepEqual(problems, [], `${t.name}: ${problems.join('; ')}`)
    }
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('compileParameterSchema: object-root output with required-must-be-true', async () => {
  const { compileParameterSchema } = await import(pathToFileURL(join(ROOT, 'src', 'schema.js')).href)
  const out = compileParameterSchema({
    a: { type: 'string', required: true, description: 'A' },
    b: { type: 'integer' },
    nested: {
      type: 'object',
      properties: { x: { type: 'string', required: true } },
    },
    list: { type: 'array', items: { type: 'string' } },
  })
  assert.equal(out.type, 'object')
  assert.deepEqual(out.required, ['a'])
  assert.equal(out.properties.a.type, 'string')
  assert.equal(out.properties.b.type, 'integer')
  assert.deepEqual(out.properties.nested.required, ['x'])
  assert.equal(out.properties.list.items.type, 'string')
})

test('compileParameterSchema: rejects required:false and bad types', async () => {
  const { compileParameterSchema } = await import(`${pathToFileURL(join(ROOT, 'src', 'schema.js')).href}?t=${Date.now()}`)
  assert.throws(() => compileParameterSchema({ a: { type: 'string', required: false } }), /required:false is illegal/)
  assert.throws(() => compileParameterSchema({ a: { type: 'datetime' } }), /unsupported type/)
  assert.throws(() => compileParameterSchema(null), /must be a JSON Schema of type object/)
})
