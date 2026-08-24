/**
 * L-008 ctl tests: install idempotency, promotion gate enforcement,
 * rollback safety. Runs the real ctl as a child process against a temp
 * DSH_HOME — never touches the real preset root.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const CTL = join(ROOT, 'bin', 'phoenix-harness-v3.mjs')
const REPO = resolve(ROOT, '..', '..')

function runCtl(args, dshHome, extraEnv = {}) {
  try {
    return {
      code: 0,
      out: execFileSync(process.execPath, [CTL, ...args], {
        env: { ...process.env, DSH_HOME: dshHome, PHOENIX_REPO: REPO, ...extraEnv },
        encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
      }),
    }
  } catch (err) {
    return { code: err.status ?? 1, out: `${err.stdout ?? ''}\n${err.stderr ?? ''}` }
  }
}

test('install canary is idempotent and writes provenance manifest', () => {
  const dsh = mkdtempSync(join(tmpdir(), 'phx-v3-dsh-'))
  try {
    const first = runCtl(['install', 'canary', '--yes'], dsh)
    assert.equal(first.code, 0, first.out.slice(-400))
    const presetDir = join(dsh, '.agent-presets', 'phoenix-v3-canary')
    assert.ok(existsSync(join(presetDir, 'agent.cordis.yml')))
    assert.ok(existsSync(join(presetDir, 'plugins', 'dsh-phoenix-harness-v3', 'lib', 'plugin.js')))
    assert.ok(existsSync(join(presetDir, 'skills', 'phoenix-context', 'SKILL.md')))
    const manifest = JSON.parse(readFileSync(join(presetDir, '.installed.json'), 'utf8'))
    assert.ok(manifest.sourceHash)
    assert.ok(manifest.pinned.harness.includes('0.1.0-rc.7'))
    // idempotent re-install (L-008)
    const second = runCtl(['install', 'canary', '--yes'], dsh)
    assert.equal(second.code, 0)
    const manifest2 = JSON.parse(readFileSync(join(presetDir, '.installed.json'), 'utf8'))
    assert.equal(manifest2.sourceHash, manifest.sourceHash)
  } finally {
    rmSync(dsh, { recursive: true, force: true })
  }
})

test('promote refuses without passing gates; rollback refuses without backup', () => {
  const dsh = mkdtempSync(join(tmpdir(), 'phx-v3-dsh2-'))
  try {
    const inst = runCtl(['install', 'canary', '--yes'], dsh)
    assert.equal(inst.code, 0)
    // no (or failing) gates -> promote refused, exit 2
    const prom = runCtl(['promote', '--yes'], dsh)
    assert.equal(prom.code, 2)
    assert.ok(/promotion refused/.test(prom.out))
    // rollback without backup refused
    const roll = runCtl(['rollback'], dsh)
    assert.equal(roll.code, 2)
    assert.ok(/no settings.yaml.phx-v3-bak/.test(roll.out))
  } finally {
    rmSync(dsh, { recursive: true, force: true })
  }
})

test('ctl never touches the production/rollback presets in a fresh DSH_HOME', () => {
  const dsh = mkdtempSync(join(tmpdir(), 'phx-v3-dsh3-'))
  try {
    runCtl(['install', 'canary', '--yes'], dsh)
    const status = runCtl(['status'], dsh)
    assert.equal(status.code, 0)
    assert.ok(/V2 rollback preset 'phoenix-v2-rollback'/.test(status.out))
    assert.ok(!existsSync(join(dsh, '.agent-presets', 'phoenix')), 'production preset must not be created by a canary install')
  } finally {
    rmSync(dsh, { recursive: true, force: true })
  }
})

test('promote (gates pass) then rollback restores the settings pointer end-to-end', () => {
  const dsh = mkdtempSync(join(tmpdir(), 'phx-v3-dsh4-'))
  try {
    // 1. install canary into the temp home
    assert.equal(runCtl(['install', 'canary', '--yes'], dsh).code, 0)
    // 2. all-passing, non-synthetic gates evidence (temp file, never the real one)
    const gates = join(dsh, 'gates-test.json')
    writeFileSync(gates, JSON.stringify({
      generatedAt: new Date().toISOString(), synthetic: false,
      gates: { correctness: true, safety: true, evidence: true, cost: true, input_reduction: true, noop_rounds: true, resume: true, restart: true, rollback: true },
    }))
    // 3. seed settings with a default pointer (the thing rollback must restore)
    const settings = join(dsh, 'settings.yaml')
    const seeded = 'agent-presets:\n  default: phoenix\npermission:\n  defaultPreset: danger-full-access\n'
    writeFileSync(settings, seeded)
    // 4. promote
    const prom = runCtl(['promote', '--yes'], dsh, { PHOENIX_GATES_FILE: gates })
    assert.equal(prom.code, 0, prom.out.slice(-400))
    assert.ok(existsSync(join(dsh, '.agent-presets', 'phoenix-v3-production', 'agent.cordis.yml')), 'production preset must be created')
    const manifest = JSON.parse(readFileSync(join(dsh, '.agent-presets', 'phoenix-v3-production', '.installed.json'), 'utf8'))
    assert.equal(manifest.promotedFrom, 'phoenix-v3-canary')
    assert.equal(manifest.gates.gates.correctness, true)
    const afterPromote = readFileSync(settings, 'utf8')
    assert.ok(/agent-presets:\s*\n\s*default:\s*"?phoenix-v3-production"?/.test(afterPromote), `settings pointer must switch: ${afterPromote}`)
    assert.ok(existsSync(join(dsh, 'settings.yaml.phx-v3-bak')), 'pre-promote settings backup required')
    // 5. rollback restores the pointer verbatim
    const roll = runCtl(['rollback'], dsh)
    assert.equal(roll.code, 0, roll.out.slice(-400))
    assert.equal(readFileSync(settings, 'utf8'), seeded, 'settings must be restored verbatim')
    // 6. production preset remains (sessions keep their preset); V2 untouched
    assert.ok(existsSync(join(dsh, '.agent-presets', 'phoenix-v3-production')))
    assert.ok(!existsSync(join(dsh, '.agent-presets', 'phoenix')), 'V2 preset never created')
  } finally {
    rmSync(dsh, { recursive: true, force: true })
  }
})
