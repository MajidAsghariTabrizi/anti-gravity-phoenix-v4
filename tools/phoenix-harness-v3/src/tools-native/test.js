/**
 * phoenix_test — canonical test runner with bounded output.
 * Uses the repo's Makefile targets (the canonical commands); full logs go
 * to artifacts, the model sees a compact summary only (Lesson: never
 * inject thousands of successful lines).
 */
import { join } from 'node:path'
import { run } from './exec-helpers.js'

const TARGETS = {
  all: { make: ['verify'], label: 'make verify (go + rust + contracts + python + secret-scan)' },
  go: { make: ['go-test'], label: 'go tests (feed-ingestor, atlas-observer, migration-runner)' },
  rust: { make: ['rust-test'], label: 'rust tests (engine, gateway, recorder, replay, executor, classifier, fork-sandbox)' },
  contract: { make: ['contract-test'], label: 'solidity contract tests' },
  python: { make: ['python-smoke'], label: 'python dashboard smoke' },
  secret_scan: { make: ['secret-scan'], label: 'secret scan' },
  harness: {
    node: ['--test', 'tools/phoenix-harness-v3/tests/*.test.mjs'],
    label: 'Phoenix Harness V3 unit tests',
  },
}

export function testTool(workspaceRoot) {
  return {
    name: 'phoenix_test',
    description:
      'Run the canonical Phoenix test suites (Makefile targets) in one bounded call. Returns compact summary: exit code, failures, warnings, timing; full logs go to an artifact file. Target: all | go | rust | contract | python | secret_scan | harness.',
    parameters: {
      target: { type: 'string', required: true, description: 'all | go | rust | contract | python | secret_scan | harness' },
      timeoutMinutes: { type: 'integer', description: 'override default timeout (1-60 min)' },
    },
    execute: async (args) => {
      const key = String(args.target ?? 'all')
      const t = TARGETS[key]
      if (!t) return `error: unknown target "${key}" (${Object.keys(TARGETS).join(' | ')})`
      const minutes = Math.min(Math.max(Number(args.timeoutMinutes ?? 20) || 20, 1), 60)
      const timeoutMs = minutes * 60000
      const started = Date.now()
      let res
      if (t.make) {
        res = await run('make', t.make, { cwd: workspaceRoot, timeoutMs, artifactTag: `test-${key}`, artifactDir: 'phoenix_test', writeArtifact: true })
      } else {
        res = await run(process.execPath, t.node, { cwd: workspaceRoot, timeoutMs, artifactTag: `test-${key}`, artifactDir: 'phoenix_test', writeArtifact: true })
      }
      const elapsed = Math.round((Date.now() - started) / 1000)
      const summary = summarizeTestOutput(res.full)
      const lines = [
        `PHOENIX_TEST ${key} — ${res.ok ? 'PASS' : 'FAIL'} (exit ${res.code}, ${elapsed}s)`,
        `target: ${t.label}`,
        `summary: ${summary}`,
        ...extractFailures(res.full),
      ]
      if (res.artifact) lines.push(`artifact (full log): ${res.artifact}`)
      return lines.join('\n')
    },
  }
}

function summarizeTestOutput(full) {
  const lines = full.split('\n').filter(Boolean)
  const resultLines = lines.filter((l) => /(^|\s)(ok|FAIL|passed|failed|error\[|warning:|\d+ passed|\d+ failed|test result)/i.test(l)).slice(-12)
  if (resultLines.length === 0) {
    return lines.slice(-6).map((l) => l.trim().slice(0, 120)).join(' | ') || '(no output)'
  }
  return resultLines.map((l) => l.trim().slice(0, 160)).join('\n  ')
}

function extractFailures(full) {
  const lines = full.split('\n')
  const fails = []
  for (let i = 0; i < lines.length; i++) {
    if (/FAILED|FAIL:|error\[E\d+\]|assertion|panicked|exit status 1/i.test(lines[i])) {
      fails.push(lines[i].trim().slice(0, 200))
      if (fails.length >= 10) break
    }
  }
  return fails.map((f) => `  FAIL: ${f}`)
}
