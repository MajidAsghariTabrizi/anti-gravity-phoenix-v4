#!/usr/bin/env node
/**
 * Phoenix Harness V3 control CLI — canonical one-command operations.
 *
 *   node bin/phoenix-harness-v3.mjs status
 *   node bin/phoenix-harness-v3.mjs install <canary|production> [--yes]
 *   node bin/phoenix-harness-v3.mjs verify            (unit tests + schema preflight + shape checks)
 *   node bin/phoenix-harness-v3.mjs promote [--yes]   (canary -> production, gate-enforced)
 *   node bin/phoenix-harness-v3.mjs rollback          (restore settings default + preset pointer)
 *   node bin/phoenix-harness-v3.mjs bench             (compaction + forensics benchmarks)
 *   node bin/phoenix-harness-v3.mjs eval <command>    (frontier eval: prepare|compare|gates)
 *
 * The DeepSeek Harness preset (~/.dsh/.agent-presets/phoenix-v3-*) is an
 * INSTALLED BUILD of tools/phoenix-harness-v3 — the repo directory is the
 * canonical source. Never edit the installed build directly.
 *
 * Never touches: the phoenix (V2) preset, shipped presets, profiles.
 */
import {
  cpSync, existsSync, mkdirSync, readFileSync, writeFileSync, readdirSync,
  statSync, rmSync,
} from 'node:fs'
import { join, resolve, dirname, basename } from 'node:path'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
// Default repo root: the parent of the canonical source dir (works when the
// ctl runs from anywhere; PHOENIX_REPO always wins).
const REPO = resolve(process.env.PHOENIX_REPO ?? join(ROOT, '..', '..'))
const DSH_HOME = process.env.DSH_HOME ?? join(process.env.USERPROFILE ?? '.', '.dsh')
const PRESETS_ROOT = join(DSH_HOME, '.agent-presets')
const SETTINGS = join(DSH_HOME, 'settings.yaml')
const V2_PRESET = 'phoenix' // CONTROL — never installed, modified, or removed
const CANARY_ID = 'phoenix-v3-canary'
const PROD_ID = 'phoenix-v3-production'
const SRC_PLUGIN = join(ROOT, 'src')
const COMPOSITION_DIR = join(ROOT, 'presets', 'phoenix-v3')
const GATES_FILE = resolve(process.env.PHOENIX_GATES_FILE ?? join(ROOT, 'reports', 'gates.json'))

function sh(cmd, args, opts = {}) {
  try {
    return { ok: true, out: execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], ...opts }) }
  } catch (err) {
    return { ok: false, out: `${err.stdout ?? ''}\n${err.stderr ?? ''}`, code: err.status }
  }
}

function versionInfo() {
  const v = readFileSync(join(ROOT, 'VERSION'), 'utf8')
    .split('\n').filter((l) => l.includes(': '))
    .map((l) => l.trim()).join(' | ')
  return v
}

function dirHash(dir) {
  const h = createHash('sha256')
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const p = join(d, e.name)
      if (e.isDirectory()) walk(p)
      else { h.update(e.name); h.update(readFileSync(p)) }
    }
  }
  if (existsSync(dir)) walk(dir)
  return h.digest('hex').slice(0, 16)
}

function manifestPath(presetId) {
  return join(PRESETS_ROOT, presetId, '.installed.json')
}

function installedManifest(presetId) {
  const p = manifestPath(presetId)
  return existsSync(p) ? JSON.parse(readFileSync(p, 'utf8')) : null
}

function installPreset(presetId) {
  const dst = join(PRESETS_ROOT, presetId)
  const srcComposition = COMPOSITION_DIR
  if (!existsSync(join(srcComposition, 'agent.cordis.yml'))) {
    console.error(`FAIL: composition source missing: ${srcComposition}`)
    process.exit(2)
  }
  const v3Hash = dirHash(ROOT)
  mkdirSync(dst, { recursive: true })
  // 1. composition + metadata + canonical skills
  for (const f of ['agent.cordis.yml', 'preset.yml']) {
    cpSync(join(srcComposition, f), join(dst, f))
  }
  const srcSkills = join(srcComposition, 'skills')
  if (existsSync(srcSkills)) cpSync(srcSkills, join(dst, 'skills'), { recursive: true })
  // 2. installed build of the canonical plugin source
  const pluginDst = join(dst, 'plugins', 'dsh-phoenix-harness-v3', 'lib')
  rmSync(pluginDst, { recursive: true, force: true })
  mkdirSync(pluginDst, { recursive: true })
  for (const e of readdirSync(SRC_PLUGIN, { withFileTypes: true })) {
    const src = join(SRC_PLUGIN, e.name)
    if (e.isFile()) cpSync(src, join(pluginDst, e.name))
    else if (e.isDirectory()) cpSync(src, join(pluginDst, e.name), { recursive: true })
  }
  // 3. reference authoring skills inherited byte-verbatim from the V2
  //    CONTROL preset (stable reference material; not part of the V3 diff)
  const v2Skills = join(PRESETS_ROOT, V2_PRESET, 'skills')
  const dstSkills = join(dst, 'skills')
  if (existsSync(v2Skills)) {
    for (const s of ['editing-cordis-compositions', 'cordis-plugin-development']) {
      const from = join(v2Skills, s)
      const to = join(dstSkills, s)
      if (existsSync(from)) { cpSync(from, to, { recursive: true }); console.log(`  skills: inherited ${s} from V2 control`) }
    }
  } else {
    console.log('  WARN: V2 control skills missing — inherited skills skipped')
  }
  // 3. installed-build manifest (provenance)
  writeFileSync(manifestPath(presetId), JSON.stringify({
    presetId,
    installedAt: new Date().toISOString(),
    sourceHash: v3Hash,
    sourceDir: ROOT,
    pinned: { harness: '@deepseek-ai/dsh ^0.1.0-rc.7', checkout: '1e7f6d9597241db0', node: process.version },
  }, null, 2))
  console.log(`INSTALLED preset ${presetId} -> ${dst}`)
  console.log(`  source hash ${v3Hash}`)
  console.log(`  NEXT: mount-validate inside a harness session with tool-cordis: agentPresets.standingKeyFor('${presetId}')`)
}

const cmd = process.argv[2] ?? 'status'
const arg = process.argv[3]
const yes = process.argv.includes('--yes')

if (cmd === 'status') {
  console.log(`PHOENIX HARNESS V3 — ${versionInfo()}`)
  console.log(`canonical source: ${ROOT}`)
  console.log(`repo:             ${REPO}`)
  for (const id of [CANARY_ID, PROD_ID]) {
    const m = installedManifest(id)
    const comp = existsSync(join(PRESETS_ROOT, id, 'agent.cordis.yml'))
    console.log(`preset ${id}: ${comp ? 'installed' : 'absent'}${m ? ` (src ${m.sourceHash} @ ${m.installedAt})` : ''}`)
  }
  console.log(`V2 CONTROL preset '${V2_PRESET}': ${existsSync(join(PRESETS_ROOT, V2_PRESET, 'agent.cordis.yml')) ? 'present (untouched)' : 'MISSING'}`)
  if (existsSync(SETTINGS)) {
    const txt = readFileSync(SETTINGS, 'utf8')
    const m = txt.match(/agent-presets:\s*\n\s*default:\s*["']?([a-z0-9-]+)/i)
    console.log(`settings default preset: ${m?.[1] ?? '(not found)'}`)
  }
} else if (cmd === 'install') {
  const id = arg === 'canary' ? CANARY_ID : arg === 'production' ? PROD_ID : null
  if (!id) { console.error('usage: install <canary|production>'); process.exit(2) }
  if (!yes) {
    console.log(`Install ${id} from canonical source ${ROOT}? Re-run with --yes to confirm.`)
    process.exit(1)
  }
  installPreset(id)
} else if (cmd === 'verify') {
  let ok = true
  const checks = [
    [join(ROOT, 'VERSION'), 'VERSION pin'],
    [join(ROOT, 'README.md'), 'README'],
    [join(COMPOSITION_DIR, 'agent.cordis.yml'), 'V3 composition'],
    [join(COMPOSITION_DIR, 'preset.yml'), 'V3 preset metadata'],
    [join(ROOT, 'knowledge', 'kernel.md'), 'kernel'],
    [join(ROOT, 'knowledge', 'freshness-policy.md'), 'freshness policy'],
    [join(ROOT, 'lessons'), 'lessons dir'],
    [join(REPO, '.phoenix-harness', 'invariants.json'), 'invariant registry (V2)'],
  ]
  for (const [p, label] of checks) {
    const present = existsSync(p)
    console.log(`${present ? 'PASS' : 'FAIL'}  ${label} ${p}`)
    if (!present) ok = false
  }
  const tests = sh(process.execPath, ['--test', join(ROOT, 'tests', '*.test.mjs')], { cwd: ROOT })
  console.log(tests.ok ? 'PASS  V3 unit tests' : `FAIL  V3 unit tests\n${tests.out.slice(-900)}`)
  if (!tests.ok) ok = false
  const preflight = sh(process.execPath, [join(ROOT, 'src', 'preflight', 'tool-schemas.mjs')], { cwd: ROOT })
  if (preflight.ok) console.log('PASS  tool schema preflight')
  else if (preflight.code === 2) console.log('SKIP  tool schema preflight (not yet wired)')
  else { console.log(`FAIL  tool schema preflight\n${preflight.out.slice(-900)}`); ok = false }
  const v2Comp = join(DSH_HOME, '.agent-presets', V2_PRESET, 'agent.cordis.yml')
  const instDir = join(DSH_HOME, '.agent-presets', CANARY_ID)
  const compArgs = [join(ROOT, 'src', 'preflight', 'validate-composition.mjs')]
  if (existsSync(v2Comp)) compArgs.push('--v2', v2Comp)
  if (existsSync(instDir)) compArgs.push('--installed', instDir)
  const comp = sh(process.execPath, compArgs, { cwd: ROOT })
  if (comp.ok) console.log('PASS  composition preflight (harness parser + strict configs + control parity)')
  else { console.log(`FAIL  composition preflight\n${comp.out.slice(-900)}`); ok = false }
  console.log(ok ? 'VERIFY: ALL CHECKS PASSED' : 'VERIFY: FAILURES PRESENT')
  process.exitCode = ok ? 0 : 1
} else if (cmd === 'promote') {
  // Promotion gate enforcement: refuse unless every gate passes.
  if (!existsSync(GATES_FILE)) {
    console.error('FAIL: promotion refused — no gates.json evidence (run eval compare first)')
    process.exit(2)
  }
  const gates = JSON.parse(readFileSync(GATES_FILE, 'utf8'))
  const failed = Object.entries(gates.gates ?? {}).filter(([, v]) => v !== true)
  if (failed.length > 0) {
    console.error(`FAIL: promotion refused — gates not passed: ${failed.map(([k]) => k).join(', ')}`)
    process.exit(2)
  }
  if (!existsSync(manifestPath(CANARY_ID))) {
    console.error('FAIL: canary preset is not installed')
    process.exit(2)
  }
  if (!yes) {
    console.log('All gates pass. Promote phoenix-v3-canary -> phoenix-v3-production? Re-run with --yes.')
    process.exit(1)
  }
  const src = join(PRESETS_ROOT, CANARY_ID)
  const dst = join(PRESETS_ROOT, PROD_ID)
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  const backup = join(PRESETS_ROOT, `${PROD_ID}.bak-${stamp}`)
  if (existsSync(dst)) { cpSync(dst, backup, { recursive: true }); console.log(`backup: ${backup}`) }
  rmSync(dst, { recursive: true, force: true })
  cpSync(src, dst, { recursive: true })
  const m = installedManifest(CANARY_ID)
  writeFileSync(manifestPath(PROD_ID), JSON.stringify({
    ...m,
    presetId: PROD_ID,
    promotedAt: new Date().toISOString(),
    promotedFrom: CANARY_ID,
    gates: gates,
  }, null, 2))
  // Switch the default preset pointer for NEW sessions (existing sessions
  // keep their own preset). The previous settings file is backed up first,
  // so `rollback` restores it verbatim.
  if (existsSync(SETTINGS)) {
    cpSync(SETTINGS, join(DSH_HOME, 'settings.yaml.phx-v3-bak'))
    const txt = readFileSync(SETTINGS, 'utf8')
    const next = txt.replace(/^(agent-presets:\s*\n(?:\s{2,}.*\n)*\s{2,}default:\s*)"?[a-z0-9-]+"?/m, `$1"${PROD_ID}"`)
    if (next !== txt) writeFileSync(SETTINGS, next)
    else { console.log('WARN: could not locate agent-presets.default — settings pointer unchanged') }
  }
  console.log(`PROMOTED ${CANARY_ID} -> ${PROD_ID} (gates: ${JSON.stringify(gates.gates)}); new sessions default to ${PROD_ID}; rollback via: phoenix-harness-v3.mjs rollback`)
} else if (cmd === 'rollback') {
  // Restore the settings default preset pointer from the last backup; never
  // touch the V2 control preset itself.
  const bak = join(DSH_HOME, 'settings.yaml.phx-v3-bak')
  if (!existsSync(bak)) {
    console.error('FAIL: no settings.yaml.phx-v3-bak backup to restore')
    process.exit(2)
  }
  cpSync(SETTINGS, join(DSH_HOME, `settings.yaml.pre-rollback-${Date.now()}`))
  cpSync(bak, SETTINGS)
  console.log(`ROLLED BACK: settings.yaml restored from ${bak}`)
  console.log('NOTE: sessions keep the preset they were created with; new sessions use the restored default.')
} else if (cmd === 'bench') {
  const bench = sh(process.execPath, [join(ROOT, 'src', 'bench', 'bench-compaction.mjs')], { cwd: ROOT })
  console.log(bench.out)
  process.exitCode = bench.ok ? 0 : 1
} else if (cmd === 'eval') {
  const sub = arg
  const runner = join(ROOT, 'src', 'eval', 'eval-runner.mjs')
  if (!existsSync(runner)) { console.error('FAIL: eval runner not implemented yet'); process.exit(2) }
  const r = sh(process.execPath, [runner, ...(sub ? [sub, ...process.argv.slice(4)] : [])], { cwd: ROOT })
  console.log(r.out)
  process.exitCode = r.ok ? 0 : 1
} else if (cmd === 'eval-live') {
  const live = join(ROOT, 'src', 'eval', 'live-runner.mjs')
  if (!existsSync(live)) { console.error('FAIL: live runner not implemented yet'); process.exit(2) }
  const r = sh(process.execPath, [live, ...process.argv.slice(3)], { cwd: ROOT })
  console.log(r.out)
  process.exitCode = r.ok ? 0 : 1
} else {
  console.log('usage: phoenix-harness-v3.mjs <status|install <canary|production> [--yes]|verify|promote [--yes]|rollback|bench|eval|eval-live ...>')
  process.exit(2)
}
