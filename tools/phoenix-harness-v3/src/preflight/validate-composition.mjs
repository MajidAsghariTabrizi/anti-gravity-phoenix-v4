#!/usr/bin/env node
/**
 * Phoenix Harness V3 — composition preflight (mount-equivalent validation).
 *
 * Uses the HARNESS'S OWN parser and health check on the V3 composition:
 *   1. js-yaml `load` with `entryListSchema` from @deepseek-ai/cordis-plugin-include
 *      (the schema carrying `!!js`) — the exact parse `dsh-agent-presets`
 *      discovery performs (`compositionProblem`);
 *   2. the exact entry-list shape check (`entryListProblem` — top-level list,
 *      every row a map with a `name`, groups recurse);
 *   3. strict config-key validation against the shipped schemas for the
 *      compaction plugins (unknown keys THROW at mount);
 *   4. plugin-row file existence (relative to the composition's directory);
 *   5. row-id parity with the V2 CONTROL composition (known-mounting oracle —
 *      the validator flags itself if it disagrees with the control).
 *
 * A pass means discovery will report the preset HEALTHY (not broken) and
 * every config key the strict loaders reject is absent. It does not replace
 * the operator's mount-validation in a live session, but it closes every
 * failure class observable without mounting.
 *
 * Usage: node validate-composition.mjs [compositionDir] [--v2 <v2CompositionPath>]
 * Exit 0 only when the composition passes every check.
 */
import { readFileSync, existsSync, statSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { createRequire } from 'node:module'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const args = process.argv.slice(2)
const v2Idx = args.indexOf('--v2')
const instIdx = args.indexOf('--installed')
const V2_COMPOSITION = v2Idx >= 0 ? resolve(args[v2Idx + 1]) : null
const INSTALLED_DIR = instIdx >= 0 ? resolve(args[instIdx + 1]) : null
const COMPOSITION_DIR = resolve(args.find((a, i) => !a.startsWith('--') && i !== v2Idx + 1 && i !== instIdx + 1) ?? join(ROOT, 'presets', 'phoenix-v3'))

// Documented V3 delta vs the control: phoenix-harness is REPLACED by
// phoenix-harness-v3 (same preset-local plugin slot, new canonical source).
const ROW_REPLACEMENTS = { 'phoenix-harness': 'phoenix-harness-v3' }

const CHECKOUT = process.env.DSH_CHECKOUT
  ?? 'C:/Users/ma.asghari/AppData/Local/npm-cache/_npx/1e7f6d9597241db0/node_modules'
const INCLUDE_PKG = join(CHECKOUT, '@deepseek-ai', 'cordis-plugin-include', 'lib', 'index.js')
const JSYAML_PKG = join(CHECKOUT, 'js-yaml')

let entryListSchema = null
let yamlLoad = null
if (existsSync(INCLUDE_PKG)) {
  try {
    const mod = await import(pathToFileURL(INCLUDE_PKG).href)
    entryListSchema = mod.entryListSchema
    const require = createRequire(import.meta.url)
    const jsyaml = require(JSYAML_PKG)
    yamlLoad = jsyaml.load
    console.log('PASS  harness parser available (cordis-plugin-include entryListSchema + js-yaml)')
  } catch (err) {
    console.log(`WARN  harness parser unavailable: ${err.message} — running structural checks only`)
  }
}

function entryListProblem(rows, at = '') {
  if (!Array.isArray(rows)) return at === '' ? 'the composition must be a top-level list of plugin rows' : `group ${at} must hold a list of plugin rows`
  for (const [index, row] of rows.entries()) {
    const label = at === '' ? `row ${String(index + 1)}` : `${at} row ${String(index + 1)}`
    if (typeof row !== 'object' || row === null || Array.isArray(row)) return `${label} is not a plugin row (expected a map with a "name")`
    const { name, group, config } = row
    if (typeof name !== 'string' || name === '') return `${label} names no plugin (a "name" string is required)`
    if (group === true) {
      const nested = entryListProblem(config, label)
      if (nested !== void 0) return nested
    }
  }
}

function compositionProblem(path) {
  let content
  try { content = readFileSync(path, 'utf8') } catch { return `the composition file cannot be read` }
  let rows
  try { rows = yamlLoad(content, { schema: entryListSchema }) } catch (error) { return `not valid YAML: ${(error instanceof Error ? error.message : String(error)).replace(/\n[\s\S]*$/, '')}` }
  return entryListProblem(rows)
}

function loadRows(path) {
  const content = readFileSync(path, 'utf8')
  return yamlLoad(content, { schema: entryListSchema })
}

function flattenRows(rows, out = []) {
  for (const row of rows) {
    out.push(row)
    if (row.group === true && Array.isArray(row.config)) flattenRows(row.config, out)
  }
  return out
}

function rowIds(rows) {
  return flattenRows(rows).map((r) => r.id).filter(Boolean).sort()
}

const STRICT_SCHEMAS = {
  '@deepseek-ai/dsh-compaction-basic': {
    keys: ['thresholdRatio', 'retainRatio', 'retainTokens', 'summarizationProvider', 'summarizationModel', 'maxTokens', 'compactionRetries', 'maxOverflowRetries', 'modelPolicies', 'auto'],
    validate(cfg) {
      const problems = []
      if (cfg.retainRatio !== undefined && cfg.retainTokens !== undefined) problems.push('retainRatio and retainTokens are mutually exclusive')
      if (cfg.thresholdRatio !== undefined && (typeof cfg.thresholdRatio !== 'number' || cfg.thresholdRatio <= 0 || cfg.thresholdRatio > 1)) problems.push(`thresholdRatio ${cfg.thresholdRatio} must be in (0, 1]`)
      if (cfg.retainTokens !== undefined) {
        if (!Number.isInteger(cfg.retainTokens) || cfg.retainTokens < 0) problems.push(`retainTokens ${cfg.retainTokens} must be a non-negative integer`)
        // retainTokens must be < threshold tokens for every plausible context
        // window (1M routed default); runtime rejects retainTokens >= threshold.
        if (cfg.thresholdRatio !== undefined && cfg.retainTokens >= Math.floor(1000000 * cfg.thresholdRatio)) problems.push(`retainTokens ${cfg.retainTokens} >= threshold tokens ${Math.floor(1000000 * cfg.thresholdRatio)} at a 1M window — compaction would be disabled with warnings`)
      }
      if (cfg.maxTokens !== undefined && (!Number.isInteger(cfg.maxTokens) || cfg.maxTokens < 1)) problems.push(`maxTokens ${cfg.maxTokens} must be a positive integer`)
      if (cfg.compactionRetries !== undefined && (!Number.isInteger(cfg.compactionRetries) || cfg.compactionRetries < 0)) problems.push(`compactionRetries must be a non-negative integer`)
      const p = cfg.summarizationProvider
      const m = cfg.summarizationModel
      if ((p === undefined) !== (m === undefined)) problems.push('summarizationProvider and summarizationModel must be set together')
      if (p !== undefined && (p.length === 0) !== (m.length === 0)) problems.push('summarization pair must be both empty or both non-empty')
      return problems
    },
  },
  '@deepseek-ai/dsh-compaction-tool-result-pruner': {
    keys: ['thresholdChars', 'headChars', 'tailChars'],
    validate(cfg) {
      const problems = []
      for (const k of ['thresholdChars', 'headChars', 'tailChars']) {
        if (cfg[k] !== undefined && (!Number.isInteger(cfg[k]) || cfg[k] < 0)) problems.push(`${k} must be a non-negative integer`)
      }
      return problems
    },
  },
}

let failures = 0
function check(label, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${ok ? '' : ` — ${detail}`}`)
  if (!ok) failures += 1
}

// 1. harness-parser health check (same code path as discovery)
function validateDir(dir, label, { expectPluginFile = false } = {}) {
  const path = join(dir, 'agent.cordis.yml')
  check(`${label}: composition readable`, existsSync(path))
  const problem = yamlLoad ? compositionProblem(path) : 'parser unavailable'
  check(`${label}: passes harness health check`, problem === undefined, problem ?? '')
  if (problem !== undefined || !yamlLoad) return null
  const dirRows = loadRows(path)
  // 2. strict config schemas for every row
  for (const row of flattenRows(dirRows)) {
    if (!row.config || typeof row.config !== 'object') continue
    const schema = STRICT_SCHEMAS[row.name]
    if (!schema) continue
    const unknown = Object.keys(row.config).filter((k) => !schema.keys.includes(k))
    check(`${label}: config keys ${row.id} (${row.name})`, unknown.length === 0, `unknown key(s): ${unknown.join(', ')}`)
    const problems = schema.validate(row.config)
    check(`${label}: config values ${row.id}`, problems.length === 0, problems.join('; '))
  }
  // 3. plugin row: module file exists relative to the composition directory
  if (expectPluginFile) {
    for (const row of flattenRows(dirRows)) {
      if (typeof row.name === 'string' && (row.name.startsWith('./') || row.name.startsWith('../'))) {
        const target = join(dir, row.name.replace(/^\.\//, ''))
        check(`${label}: plugin file ${row.id} -> ${row.name}`, existsSync(target) && statSync(target).isFile(), target)
      }
    }
  }
  return dirRows
}

const rows = validateDir(COMPOSITION_DIR, 'V3 canonical', { expectPluginFile: false })
if (INSTALLED_DIR && existsSync(INSTALLED_DIR)) {
  console.log('')
  validateDir(INSTALLED_DIR, 'V3 installed', { expectPluginFile: true })
} else if (INSTALLED_DIR) {
  check('V3 installed preset exists', false, INSTALLED_DIR)
}

// 4. row-id parity vs V2 control oracle (control must pass too — flags the validator)
if (rows !== null && V2_COMPOSITION && existsSync(V2_COMPOSITION) && yamlLoad) {
  const v2Problem = compositionProblem(V2_COMPOSITION)
  check('V2 CONTROL passes the same health check (oracle)', v2Problem === undefined, v2Problem ?? '')
  if (v2Problem === undefined) {
    const v2 = rowIds(loadRows(V2_COMPOSITION))
    const v3 = rowIds(rows)
    const onlyInV3 = v3.filter((id) => !v2.includes(id))
    const missingFromV3 = v2.filter((id) => !v3.includes(id) && !(ROW_REPLACEMENTS[id] && v3.includes(ROW_REPLACEMENTS[id])))
    check('V3 keeps every control row (or documented replacement)', missingFromV3.length === 0, `missing: ${missingFromV3.join(', ')}`)
    const allowedNew = Object.values(ROW_REPLACEMENTS)
    const unexpected = onlyInV3.filter((id) => !allowedNew.includes(id))
    check('V3 new rows are the documented deltas only', unexpected.length === 0, `unexpected: ${unexpected.join(', ')}`)
  }
} else if (!V2_COMPOSITION) {
  console.log('SKIP  row-parity vs V2 control (no --v2 path given)')
}

console.log(`\nCOMPOSITION PREFLIGHT: ${failures} failures`)
process.exit(failures === 0 ? 0 : 1)
