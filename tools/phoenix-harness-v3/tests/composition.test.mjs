/**
 * Composition preflight regression: the V3 composition must pass the
 * harness's own parser + health check (entryListSchema), the strict
 * compaction config schemas, plugin-file resolution for the installed
 * build, and row parity with the V2 control (the oracle — if the control
 * fails the same checks, the validator itself is wrong).
 *
 * Skips (not fails) when the pinned DSH checkout is unreachable.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const VALIDATOR = join(ROOT, 'src', 'preflight', 'validate-composition.mjs')
const CHECKOUT = process.env.DSH_CHECKOUT
  ?? 'C:/Users/ma.asghari/AppData/Local/npm-cache/_npx/1e7f6d9597241db0/node_modules'

test('V3 composition passes the harness health check and control parity', { skip: !existsSync(join(CHECKOUT, '@deepseek-ai', 'cordis-plugin-include', 'lib', 'index.js')) && 'checkout unreachable' }, () => {
  const dshHome = process.env.DSH_HOME ?? join(process.env.USERPROFILE ?? '.', '.dsh')
  const v2 = join(dshHome, '.agent-presets', 'phoenix', 'agent.cordis.yml')
  const inst = join(dshHome, '.agent-presets', 'phoenix-v3-canary')
  const args = [VALIDATOR]
  if (existsSync(v2)) args.push('--v2', v2)
  if (existsSync(inst)) args.push('--installed', inst)
  let out = ''
  try {
    out = execFileSync(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' })
  } catch (err) {
    out = `${err.stdout ?? ''}\n${err.stderr ?? ''}`
    assert.fail(`composition preflight failed:\n${out.slice(-1200)}`)
  }
  assert.match(out, /COMPOSITION PREFLIGHT: 0 failures/)
  assert.match(out, /passes harness health check/)
})

test('validator flags itself when the control oracle fails (defect injection)', { skip: !existsSync(join(CHECKOUT, '@deepseek-ai', 'cordis-plugin-include', 'lib', 'index.js')) && 'checkout unreachable' }, async () => {
  // Point --v2 at a syntactically broken control: the parity oracle must fail,
  // proving the check is not vacuously green.
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-comp-'))
  try {
    writeFileSync(join(tmp, 'broken-v2.yml'), 'not: [valid composition\n')
    let threw = false
    try {
      execFileSync(process.execPath, [VALIDATOR, join(ROOT, 'presets', 'phoenix-v3'), '--v2', join(tmp, 'broken-v2.yml')], { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8' })
    } catch { threw = true }
    assert.ok(threw, 'broken control oracle must fail the preflight')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})
