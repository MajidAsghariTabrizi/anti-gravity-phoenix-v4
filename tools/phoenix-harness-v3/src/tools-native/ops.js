/**
 * Phoenix Harness V3 — operations native tools (Phase 3B/3G/3H):
 *
 *   phoenix_current_truth  — authoritative compact capsule (typed JSON,
 *                            normal <=8K chars, hard <=12K; incremental).
 *   phoenix_changed_surface— one-call working-tree delta (status/diff/untracked).
 *   phoenix_test_matrix    — test inventory + run commands in one call.
 *   phoenix_ci_snapshot    — one-shot CI state for a branch/SHA (no waiting).
 *   phoenix_wait           — deterministic wait INSIDE the tool (model
 *                            suspended; wakes on state change or fails
 *                            closed at the deadline; zero polling rounds).
 *
 * All are read-only except the current-truth capsule file (workspace-local
 * durable state, gitignored) and are idempotent, bounded, and fail-closed.
 */
import { existsSync, readFileSync, writeFileSync, mkdirSync, readdirSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { run, writeJsonArtifact, sha16 } from './exec-helpers.js'
import { waitForState } from '../wait.js'

const TRUTH_FILE = '.phoenix-harness/CURRENT_TRUTH.json'
const TRUTH_NORMAL_CAP = 8000 // chars (serialized) — normal target
const TRUTH_HARD_CAP = 12000 // chars — refuse beyond
const LIST_CAP = 40
const FIELD_CAP = 300

function cap(s, n = FIELD_CAP) {
  const v = String(s ?? '')
  return v.length > n ? v.slice(0, n) : v
}

function truthPath(root) {
  return join(root, ...TRUTH_FILE.split('/'))
}

function readTruth(root) {
  const p = truthPath(root)
  if (!existsSync(p)) return null
  try {
    const t = JSON.parse(readFileSync(p, 'utf8'))
    if (t?.schema !== 'phoenix.current-truth.v1') return null
    return t
  } catch {
    return null
  }
}

function truthSize(t) {
  return JSON.stringify(t).length
}

function verdictFor(t) {
  if (!t?.updatedAt) return 'unset'
  const ageMin = (Date.now() - new Date(t.updatedAt).getTime()) / 60000
  return ageMin < 30 ? 'fresh' : ageMin < 240 ? 'aging' : 'stale'
}

export function currentTruthTool(workspaceRoot) {
  return {
    name: 'phoenix_current_truth',
    description:
      'The authoritative compact current-truth capsule (typed JSON at .phoenix-harness/CURRENT_TRUTH.json; normal <=8K chars, hard cap 12K). get: read it (with freshness verdict vs git HEAD); update: incrementally set scalars (mission, nextAction, production) or merge lists (decisions, filesChanged, tests, ci, blockers, evidence) — the repo SHA/branch are refreshed automatically. Use at mission start and at phase boundaries instead of re-reading historical reports.',
    parameters: {
      action: { type: 'string', required: true, description: 'get | update' },
      mission: { type: 'string', description: 'update: active mission (one line)' },
      nextAction: { type: 'string', description: 'update: exact next action' },
      production: { type: 'string', description: 'update: current Production status line (read-only truth only)' },
      decisions: { type: 'array', items: { type: 'string' }, description: 'update: merge decision strings' },
      filesChanged: { type: 'array', items: { type: 'string' }, description: 'update: merge changed-file entries' },
      tests: { type: 'array', items: { type: 'string' }, description: 'update: merge test-result entries' },
      ci: { type: 'array', items: { type: 'string' }, description: 'update: merge CI entries' },
      blockers: { type: 'array', items: { type: 'string' }, description: 'update: merge blocker entries ([] to clear)' },
      evidence: { type: 'array', items: { type: 'string' }, description: 'update: merge evidence references' },
    },
    execute: async (args) => {
      const cur = readTruth(workspaceRoot)
      const gitSha = await run('git', ['-C', workspaceRoot, 'rev-parse', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
      const gitBranch = await run('git', ['-C', workspaceRoot, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
      const sha = gitSha.ok ? gitSha.stdout.split('\n')[0] : null
      const branch = gitBranch.ok ? gitBranch.stdout.split('\n')[0] : null

      if (args.action === 'get') {
        if (!cur) return 'error: no current-truth capsule yet — phoenix_current_truth action=update first'
        const stale = sha && cur.repo?.sha && cur.repo.sha !== sha
        const lines = [
          `CURRENT TRUTH — ${verdictFor(cur)}${stale ? ' (recorded SHA != git HEAD — refresh by updating)' : ''}`,
          `repo: ${cur.repo?.sha ?? '?'} ${cur.repo?.branch ?? ''}`,
          `updatedAt: ${cur.updatedAt ?? '?'}`,
          `mission: ${cur.mission ?? '(unset)'}`,
          `production: ${cur.production ?? '(unset)'}`,
          `nextAction: ${cur.nextAction ?? '(unset)'}`,
          ...(cur.blockers?.length ? [`blockers: ${cur.blockers.length}`] : []),
          ...cur.blockers.map((b) => `  - ${b}`),
          `sections: decisions=${cur.decisions?.length ?? 0} filesChanged=${cur.filesChanged?.length ?? 0} tests=${cur.tests?.length ?? 0} ci=${cur.ci?.length ?? 0} evidence=${cur.evidence?.length ?? 0}`,
          `size: ${truthSize(cur)} chars (normal<=8K, hard<=12K)`,
        ]
        return lines.join('\n')
      }

      if (args.action !== 'update') return 'error: action must be get | update'
      const base = cur ?? {
        schema: 'phoenix.current-truth.v1',
        createdAt: new Date().toISOString(),
        repo: {},
        mission: null, production: null, nextAction: null,
        decisions: [], filesChanged: [], tests: [], ci: [], blockers: [], evidence: [],
      }
      const next = {
        ...base,
        updatedAt: new Date().toISOString(),
        repo: { sha: sha ?? base.repo?.sha ?? null, branch: branch ?? base.repo?.branch ?? null },
      }
      for (const k of ['mission', 'nextAction', 'production']) {
        if (args[k] !== undefined) next[k] = cap(args[k])
      }
      const merge = (key) => {
        const vals = Array.isArray(args[key]) ? args[key].map((v) => cap(v)) : null
        if (vals === null) return
        if (vals.length === 0) { next[key] = []; return }
        const seen = new Set((next[key] ?? []).slice(-LIST_CAP))
        for (const v of vals) if (!seen.has(v)) { seen.add(v); next[key].push(v) }
        next[key] = next[key].slice(-LIST_CAP)
      }
      for (const k of ['decisions', 'filesChanged', 'tests', 'ci', 'blockers', 'evidence']) merge(k)
      next.blockers = (next.blockers ?? []).slice(0, 12)
      const size = truthSize(next)
      if (size > TRUTH_HARD_CAP) {
        return `error: capsule would be ${size} chars (> hard cap ${TRUTH_HARD_CAP}) — refused; trim lists first`
      }
      try {
        const p = truthPath(workspaceRoot)
        mkdirSync(join(p, '..'), { recursive: true })
        writeFileSync(p, JSON.stringify(next, null, 2))
      } catch (err) {
        return `error: cannot write capsule: ${String(err?.message ?? err)}`
      }
      const warn = size > TRUTH_NORMAL_CAP ? ` (WARN: above normal 8K target — prune next update)` : ''
      return `current truth updated${warn}: sha=${next.repo.sha ?? '?'} branch=${next.repo.branch ?? '?'} size=${size} chars`
    },
  }
}

export function changedSurfaceTool(workspaceRoot) {
  return {
    name: 'phoenix_changed_surface',
    description:
      'One-call working-tree delta: branch/SHA, porcelain status (capped), unstaged+staged diff stat, changed/untracked name lists. Replaces multiple git calls before commits/PRs. Read-only.',
    parameters: {
      limit: { type: 'integer', description: 'max lines per section (default 40)' },
    },
    execute: async (args) => {
      const limit = Math.min(Number(args.limit ?? 40) || 40, 100)
      const git = async (a) => run('git', ['-C', workspaceRoot, ...a], { timeoutMs: 20000, writeArtifact: false })
      const [branch, sha, status, diffStat, names, untracked] = await Promise.all([
        git(['rev-parse', '--abbrev-ref', 'HEAD']),
        git(['rev-parse', 'HEAD']),
        git(['status', '--porcelain']),
        git(['diff', '--stat']),
        git(['diff', '--name-only']),
        git(['ls-files', '--others', '--exclude-standard']),
      ])
      const out = {
        branch: branch.ok ? branch.stdout.split('\n')[0] : null,
        sha: sha.ok ? sha.stdout.split('\n')[0] : null,
        status: status.ok ? status.stdout.split('\n').filter(Boolean).slice(0, limit) : [],
        diffStat: diffStat.ok ? diffStat.stdout.split('\n').filter(Boolean).slice(0, limit) : [],
        changed: names.ok ? names.stdout.split('\n').filter(Boolean).slice(0, limit) : [],
        untracked: untracked.ok ? untracked.stdout.split('\n').filter(Boolean).slice(0, limit) : [],
      }
      out.dirty = out.status.length > 0
      writeJsonArtifact('changed_surface', 'surface', out)
      const lines = [
        `CHANGED SURFACE — ${out.branch ?? '?'} @ ${out.sha ?? '?'} dirty=${out.dirty}`,
        ...out.status.map((s) => `  ${s}`),
        ...(out.diffStat.length ? ['diff stat:', ...out.diffStat.map((s) => `  ${s}`)] : []),
        ...(out.untracked.length ? [`untracked (${out.untracked.length}, capped):`, ...out.untracked.map((s) => `  ${s}`)] : []),
      ]
      return lines.join('\n')
    },
  }
}

export function testMatrixTool(workspaceRoot) {
  return {
    name: 'phoenix_test_matrix',
    description:
      'One-call test inventory: the V3 suite map (knowledge/registries/tests.json), test-file inventory under tests/, and exact run commands. Replaces multiple reads before running or adding tests. Read-only.',
    parameters: {},
    execute: async () => {
      const out = { suites: [], testFiles: [], runCommands: [] }
      const reg = join(workspaceRoot, 'tools', 'phoenix-harness-v3', 'knowledge', 'registries', 'tests.json')
      if (existsSync(reg)) {
        try {
          const r = JSON.parse(readFileSync(reg, 'utf8'))
          out.suites = (r.suites ?? []).map((s) => typeof s === 'string' ? s : (s.file ?? s.name ?? JSON.stringify(s))).slice(0, 30)
        } catch { /* skip */ }
      }
      const testsDir = join(workspaceRoot, 'tools', 'phoenix-harness-v3', 'tests')
      if (existsSync(testsDir)) {
        try {
          out.testFiles = readdirSync(testsDir).filter((f) => f.endsWith('.test.mjs')).sort()
        } catch { /* skip */ }
      }
      out.runCommands = [
        'node --test tools/phoenix-harness-v3/tests/*.test.mjs',
        'node tools/phoenix-harness-v3/bin/phoenix-harness-v3.mjs verify',
      ]
      writeJsonArtifact('test_matrix', 'matrix', out)
      const lines = [
        `TEST MATRIX — ${out.testFiles.length} suites`,
        ...out.testFiles.map((f) => `  ${f}`),
        `run: ${out.runCommands.join(' | ')}`,
      ]
      return lines.join('\n')
    },
  }
}

export function ciSnapshotTool(workspaceRoot) {
  return {
    name: 'phoenix_ci_snapshot',
    description:
      'One-shot CI state (no waiting): recent GitHub Actions runs for the current branch or a SHA (databaseId, workflow, status, conclusion, headSha). Use phoenix_ci_watch to block until a change instead of polling. Read-only.',
    parameters: {
      branch: { type: 'string', description: 'branch filter (default: current branch)' },
      sha: { type: 'string', description: 'exact head SHA filter (overrides branch)' },
      limit: { type: 'integer', description: 'max runs (default 8)' },
    },
    execute: async (args) => {
      const limit = Math.min(Number(args.limit ?? 8) || 8, 20)
      const branchArg = args.sha
        ? []
        : [args.branch ? String(args.branch) : (await run('git', ['-C', workspaceRoot, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })).stdout?.split('\n')[0]]
      const res = await run('gh', [
        'run', 'list', ...(branchArg.length ? ['--branch', branchArg[0]] : []),
        ...(args.sha ? ['--commit', String(args.sha)] : []),
        '--limit', String(limit),
        '--json', 'databaseId,name,workflowName,status,conclusion,createdAt,headSha,event',
      ], { timeoutMs: 30000, writeArtifact: false })
      if (!res.ok) return `error: gh run list failed (${res.code}) — CI snapshot unavailable; fail-closed`
      let runs = []
      try { runs = JSON.parse(res.stdout) } catch { return 'error: gh returned non-JSON — CI snapshot unavailable; fail-closed' }
      writeJsonArtifact('ci_snapshot', 'ci', { runs })
      const lines = [`CI SNAPSHOT — ${runs.length} runs`, ...runs.map((r) =>
        `${r.databaseId} ${r.workflowName} status=${r.status} conclusion=${r.conclusion ?? '-'} sha=${r.headSha.slice(0, 8)} ${r.createdAt}`)]
      return lines.join('\n')
    },
  }
}

export function waitTool(governor, workspaceRoot, sidOf) {
  return {
    name: 'phoenix_wait',
    description:
      'Deterministic wait INSIDE the tool — the model is suspended and wakes on state change; zero polling rounds. target file: wait until path exists; target content: wait until path contains a substring; target timeout: fixed bounded sleep. Fails closed at the deadline. Use instead of job_output polling loops.',
    parameters: {
      target: { type: 'string', required: true, description: 'file | content | timeout' },
      path: { type: 'string', description: 'file path relative to the workspace (file/content targets)' },
      contains: { type: 'string', description: 'substring to wait for (content target)' },
      timeoutMs: { type: 'integer', description: 'deadline ms (default 300000, max 120min)' },
      intervalMs: { type: 'integer', description: 'check interval ms (default 2000, min 500, max 30000)' },
    },
    execute: async (args, exec) => {
      const target = String(args.target ?? 'file')
      const timeoutMs = Math.min(Number(args.timeoutMs ?? 300000) || 300000, 120 * 60000)
      const intervalMs = Math.min(Math.max(Number(args.intervalMs ?? 2000) || 2000, 500), 30000)
      const id = `wait-${sha16(`${target}:${args.path ?? ''}:${args.contains ?? ''}`)}`
      const sid = sidOf ? sidOf(exec) : 'unknown'
      const deadlineMs = Date.now() + timeoutMs
      const reason = `wait ${target} ${args.path ?? ''}`
      governor?.registerWait?.(sid, id, deadlineMs, reason)

      const check = async () => {
        if (target === 'timeout') return Date.now() >= deadlineMs ? { elapsed: 'deadline' } : null
        const p = join(workspaceRoot, ...String(args.path ?? '').split('/').filter(Boolean))
        if (target === 'file') {
          return existsSync(p) ? { path: args.path, size: statSync(p).size } : null
        }
        if (target === 'content') {
          if (!existsSync(p)) return null
          const text = readFileSync(p, 'utf8')
          const needle = String(args.contains ?? '')
          if (!needle) return { path: args.path, contains: null, matched: true }
          return text.includes(needle) ? { path: args.path, matched: true } : null
        }
        return { error: `unknown target ${target}` }
      }

      let result
      if (target === 'file' || target === 'content') {
        result = await waitForState(check, { intervalMs, maxWaitMs: timeoutMs, failClosedMessage: `wait deadline reached (${timeoutMs}ms) — fail closed` })
      } else if (target === 'timeout') {
        result = await waitForState(check, { intervalMs, maxWaitMs: timeoutMs, failClosedMessage: 'timeout wait deadline' })
      } else {
        governor?.clearWait?.(sid, id)
        return 'error: target must be file | content | timeout'
      }
      governor?.clearWait?.(sid, id)
      if (!result.ok) return `WAIT FAILED: ${result.error} (waited ${result.waitedMs}ms, ${result.checks} checks)`
      return `WAIT OK: ${JSON.stringify(result.state)} — ${result.waitedMs}ms, ${result.checks} internal checks, 0 model polling rounds`
    },
  }
}
