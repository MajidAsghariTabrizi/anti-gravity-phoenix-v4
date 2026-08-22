#!/usr/bin/env node
/**
 * Phoenix Harness V3 — provider-request preflight (Lesson L-002).
 *
 * Serializes every phoenix_* tool exactly the way the DeepSeek adapter does
 * and validates each parameters schema with the installed harness boundary
 * (assertSupportedJsonSchema) plus structural assertions: object root,
 * no null types, required declared. Exit 0 only when every tool schema is
 * provider-safe.
 */
import { existsSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { pathToFileURL } from 'node:url'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const CHECKOUT = process.env.DSH_CHECKOUT
  ?? 'C:/Users/ma.asghari/AppData/Local/npm-cache/_npx/1e7f6d9597241db0/node_modules/@deepseek-ai'
const PLUGIN = join(ROOT, 'src', 'plugin.js')

if (!existsSync(PLUGIN)) {
  console.error(`plugin not found: ${PLUGIN}`)
  process.exit(2)
}

const harnessReachable = existsSync(join(CHECKOUT, 'dsh-tools', 'lib', 'index.js'))
let assertSupportedJsonSchema = null
if (harnessReachable) {
  const mod = await import(pathToFileURL(join(CHECKOUT, 'dsh-tools', 'lib', 'index.js')).href)
  assertSupportedJsonSchema = mod.assertSupportedJsonSchema
} else {
  console.log('WARN: installed dsh-tools not reachable — running structural checks only')
}

const plugin = await import(pathToFileURL(PLUGIN).href)
const definitions = []
const ctx = {
  on() { return () => {} },
  get() { return undefined },
  tools: { register(def) { definitions.push(def) } },
}
plugin.apply(ctx, { workspaceRoot: process.cwd() })

const phoenixTools = definitions.filter((d) => d.name.startsWith('phoenix_'))
if (phoenixTools.length === 0) {
  console.error('FAIL: no phoenix_* tools registered')
  process.exit(1)
}

let failures = 0
for (const tool of phoenixTools) {
  const wire = { type: 'function', function: { name: tool.name, description: tool.description, parameters: tool.parameters } }
  const params = tool.parameters
  const problems = []
  if (!params || typeof params !== 'object') problems.push('parameters missing')
  if (params?.type !== 'object') problems.push(`root type is ${JSON.stringify(params?.type)} (must be "object")`)
  if (params && (!params.properties || typeof params.properties !== 'object')) problems.push('properties missing')
  if (params?.required !== undefined && !Array.isArray(params.required)) problems.push('required is not an array')
  const nullHits = []
  const walk = (node, path) => {
    if (Array.isArray(node)) { node.forEach((v, i) => walk(v, `${path}[${i}]`)); return }
    if (node !== null && typeof node === 'object') {
      for (const [k, v] of Object.entries(node)) {
        if (k === 'type' && (v === null || v === undefined)) nullHits.push(`${path}.type`)
        walk(v, path ? `${path}.${k}` : k)
      }
    }
  }
  walk(params, `${tool.name}.parameters`)
  if (nullHits.length > 0) problems.push(...nullHits)
  if (harnessReachable) {
    try {
      assertSupportedJsonSchema(params)
    } catch (err) {
      problems.push(`harness boundary: ${err.message}`)
    }
  }
  if (problems.length > 0) {
    failures += 1
    console.log(`FAIL ${tool.name}`)
    for (const p of problems) console.log(`     ${p}`)
  } else {
    console.log(`PASS ${tool.name} — type=${params.type}, props=${Object.keys(params.properties ?? {}).length}, required=${(params.required ?? []).join(',') || '-'}`)
  }
}

console.log(`\nPREFLIGHT: ${phoenixTools.length} phoenix tools checked, ${failures} failures`)
process.exit(failures === 0 ? 0 : 1)
