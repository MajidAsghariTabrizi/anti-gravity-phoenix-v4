/**
 * Phoenix Harness V3 eval — campaign mechanics (Phase 2/4).
 *
 * Pure-ish, DSH-free utilities for the live A/B runner: checkout pin
 * verification, arm-home preparation (isolated DSH_HOME per arm), temp
 * worktree creation at the pinned SHA, per-task fixture planting, machine
 * context copies, telemetry collection, and PR/branch cleanup.
 *
 * Every spawn is an args-array spawn (Lesson L-003); every path is bounded
 * and fail-closed; no secrets are ever read or printed.
 */
import { execFile } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync, copyFileSync, cpSync, rmSync, readdirSync, statSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..')
export const DEFAULT_CHECKOUT = join(process.env.USERPROFILE ?? '.', 'AppData', 'Local', 'npm-cache', '_npx', '1e7f6d9597241db0')
export const PINNED_VERSION = '0.1.0-rc.7'

/** Run a command with an args array; {ok, code, stdout, stderr} — never throws. */
export function run(cmd, args, opts = {}) {
  const timeoutMs = opts.timeoutMs ?? 60000
  return new Promise((resolveFn) => {
    let settled = false
    const settle = (r) => { if (!settled) { settled = true; resolveFn(r) } }
    try {
      execFile(cmd, args, {
        cwd: opts.cwd ?? REPO_ROOT,
        timeout: timeoutMs,
        maxBuffer: opts.maxBuffer ?? 8 * 1024 * 1024,
        windowsHide: true,
        env: { ...process.env, ...(opts.env ?? {}) },
      }, (error, stdout, stderr) => {
        if (error) settle({ ok: false, code: error.code ?? 1, stdout: String(stdout ?? ''), stderr: String(stderr ?? '') })
        else settle({ ok: true, code: 0, stdout: String(stdout ?? ''), stderr: String(stderr ?? '') })
      })
    } catch (err) {
      settle({ ok: false, code: 'ENOENT', stdout: '', stderr: String(err?.message ?? err) })
    }
  })
}

export function checkoutPath() {
  return resolve(process.env.PHOENIX_DSH_CHECKOUT ?? DEFAULT_CHECKOUT)
}

/** Verify the pinned checkout exists and its package version matches the pin. */
export function verifyCheckout(checkout = checkoutPath()) {
  const pkg = join(checkout, 'node_modules', '@deepseek-ai', 'dsh', 'package.json')
  if (!existsSync(pkg)) return { ok: false, error: `pinned checkout package missing: ${pkg}` }
  try {
    const v = JSON.parse(readFileSync(pkg, 'utf8')).version
    if (v !== PINNED_VERSION) return { ok: false, error: `checkout version ${v} != pin ${PINNED_VERSION}` }
    return { ok: true, version: v, checkout }
  } catch (err) {
    return { ok: false, error: `cannot read checkout package: ${err?.message ?? err}` }
  }
}

export function realDshHome() {
  return process.env.DSH_HOME ? resolve(process.env.DSH_HOME) : join(process.env.USERPROFILE ?? '.', '.dsh')
}

export function presetIdFor(arm) {
  if (arm === 'control') return 'phoenix'
  if (arm === 'candidate') return 'phoenix-v3-canary'
  if (arm === 'judge') return '' // bare agent: no preset
  return null
}

export function toolsModeFor(arm) {
  return arm === 'candidate' ? 'code' : undefined // DSH_TOOLS_MODE env seam (rc.7)
}

export function armHomeFor(campaignRoot, arm) {
  return join(campaignRoot, 'arms', arm)
}

/**
 * Prepare an isolated arm DSH_HOME:
 *  - control: copies settings.yaml, .credentials.yaml, storages/, and the
 *    untouched `phoenix` (V2) preset from the real home.
 *  - candidate: copies settings/.credentials/storages + the V2 preset (for
 *    install parity checks) and runs `ctl install canary --yes` into the arm
 *    home so the candidate is the CURRENT canonical source, not a stale copy.
 *  - judge: settings + credentials only (bare agents, no preset).
 */
export async function prepareArmHome(armHome, arm, { realHome = realDshHome() } = {}) {
  mkdirSync(armHome, { recursive: true })
  const copy = (rel) => {
    const src = join(realHome, rel)
    const dst = join(armHome, rel)
    if (existsSync(src)) {
      if (statSync(src).isDirectory()) cpSync(src, dst, { recursive: true })
      else { mkdirSync(dirname(dst), { recursive: true }); copyFileSync(src, dst) }
    }
  }
  copy('settings.yaml')
  copy('.credentials.yaml')
  copy('storages')
  if (arm === 'control') copy(join('.agent-presets', 'phoenix'))
  if (arm === 'candidate') {
    copy(join('.agent-presets', 'phoenix'))
    const ctl = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'bin', 'phoenix-harness-v3.mjs')
    const res = await run(process.execPath, [ctl, 'install', 'canary', '--yes'], {
      timeoutMs: 180000,
      env: { DSH_HOME: armHome, PHOENIX_REPO: REPO_ROOT },
    })
    if (!res.ok) return { ok: false, error: `candidate install failed: ${res.stdout} ${res.stderr}` }
    const inst = join(armHome, '.agent-presets', 'phoenix-v3-canary', 'agent.cordis.yml')
    if (!existsSync(inst)) return { ok: false, error: `candidate install produced no preset at ${inst}` }
  }
  return { ok: true, armHome }
}

export function shaOf(ref = 'HEAD', cwd = REPO_ROOT) {
  return run('git', ['-C', cwd, 'rev-parse', ref], { timeoutMs: 15000 })
}

export async function createWorktree(sha, dir) {
  rmSync(dir, { recursive: true, force: true })
  const res = await run('git', ['-C', REPO_ROOT, 'worktree', 'add', '--detach', dir, sha], { timeoutMs: 120000 })
  return res.ok ? { ok: true, dir } : { ok: false, error: `${res.stdout} ${res.stderr}` }
}

export async function removeWorktree(dir) {
  await run('git', ['-C', REPO_ROOT, 'worktree', 'remove', '--force', dir], { timeoutMs: 60000 }).catch?.(() => {})
  await run('git', ['-C', REPO_ROOT, 'worktree', 'prune'], { timeoutMs: 60000 })
  rmSync(dir, { recursive: true, force: true })
}

/** Plant per-task fixtures into a worktree (deterministic, idempotent). */
export async function plantTaskFixtures(worktree, taskId) {
  const fixtureRoot = join(REPO_ROOT, 'tools', 'phoenix-harness-v3', 'benchmarks', 'frontier', 'fixtures')
  if (taskId === 'bug-fix') {
    const src = join(fixtureRoot, 'amount-math', '.planted')
    const dst = join(fixtureRoot, 'amount-math', 'buggy_amount.mjs')
    if (existsSync(src)) copyFileSync(src, dst)
    return { ok: true }
  }
  if (taskId === 'long-context') {
    const gen = join(fixtureRoot, 'long-context', 'gen-corpus.mjs')
    const res = await run(process.execPath, [gen], { timeoutMs: 30000 })
    if (!res.ok) return { ok: false, error: res.stderr || res.stdout }
    return { ok: true }
  }
  return { ok: true }
}

/** Copy machine-local context (AGENTS.local.md, .agent-private index) into a worktree. */
export function copyLocalContext(worktree) {
  const entries = ['AGENTS.local.md', join('.agent-private', '00_CONTEXT_INDEX.md')]
  for (const rel of entries) {
    const src = join(REPO_ROOT, rel)
    if (existsSync(src)) {
      mkdirSync(join(worktree, dirname(rel)), { recursive: true })
      copyFileSync(src, join(worktree, rel))
    }
  }
}

export function taskPromptText(taskDef, { sessionId, arm, taskId } = {}) {
  const frame = [
    'EVALUATION TASK (automated A/B measurement of an agent-harness preset).',
    'Complete the task below. Report facts and evidence; never fabricate.',
    `Task id: ${taskId}. You are running as the "${arm ?? 'unknown'}" arm in an isolated throwaway worktree; mutations here are local-only.`,
  ]
  return `${frame.join(' ')}\n\n${taskDef.prompt}`
}

/** Copy per-session telemetry/evidence files (bounded) into a run dir. */
export function collectSessionEvidence(roots, sessionId, dest, cap = 40 * 1024 * 1024) {
  const found = []
  let bytes = 0
  for (const root of roots) {
    if (!existsSync(root)) continue
    const walk = (base, depth) => {
      if (depth > 6 || bytes >= cap) return
      let entries
      try { entries = readdirSync(base, { withFileTypes: true }) } catch { return }
      for (const e of entries) {
        if (bytes >= cap) return
        const p = join(base, e.name)
        if (e.isDirectory()) { walk(p, depth + 1); continue }
        if (p.includes(sessionId)) {
          try {
            const size = statSync(p).size
            const dst = join(dest, 'evidence', e.name.length > 90 ? `${e.name.slice(0, 60)}-${p.split('\\').join('/').split('/').slice(-2, -1)[0] ?? 'x'}.txt` : e.name)
            mkdirSync(dirname(dst), { recursive: true })
            copyFileSync(p, dst)
            found.push({ file: p.replace(/\\/g, '/'), size })
            bytes += size
          } catch { /* bounded best-effort */ }
        }
      }
    }
    walk(root, 0)
  }
  return found
}

/** Close eval-owned PRs and delete their branches (pr-ci-delivery cleanup). */
export async function cleanupEvalPrs() {
  const list = await run('gh', ['pr', 'list', '--state', 'open', '--search', 'phoenix-eval', '--json', 'number,headRefName'], { timeoutMs: 30000 })
  if (!list.ok) return { closed: 0, deleted: 0 }
  let prs = []
  try { prs = JSON.parse(list.stdout) } catch { return { closed: 0, deleted: 0 } }
  let closed = 0
  let deleted = 0
  for (const pr of prs) {
    const c = await run('gh', ['pr', 'close', String(pr.number), '--comment', 'Automated eval cleanup — throwaway measurement branch.'], { timeoutMs: 30000 })
    if (c.ok) closed += 1
    const d = await run('git', ['-C', REPO_ROOT, 'push', 'origin', '--delete', pr.headRefName], { timeoutMs: 60000 })
    if (d.ok) deleted += 1
  }
  return { closed, deleted }
}
