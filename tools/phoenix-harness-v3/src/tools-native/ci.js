/**
 * phoenix_ci_watch / phoenix_pr_flow / phoenix_release_preflight /
 * phoenix_release_dispatch — CI and release native tools.
 *
 * phoenix_ci_watch BLOCKS INSIDE THE TOOL (model suspended) until a TERMINAL
 * CI outcome — status=completed with a real conclusion — or the deadline
 * passes (fail-closed). This is the "suspend the model, wake on terminal"
 * seam — zero polling rounds. An already-terminal run/check-set returns
 * IMMEDIATELY after one fresh read, so re-entry is cheap and never creates
 * another long wait; non-terminal transitions (queued -> in_progress) are
 * not wake reasons. A completed/failure observation is a VALID terminal
 * result, not a tool failure.
 * phoenix_release_dispatch NEVER mutates by itself: it validates
 * authorization + release-graph gates and prints the prepared MUTATION
 * PLAN; the canonical controller runs through the documented repo
 * scripts only after explicit owner approval (zero financial-authority
 * regression by construction).
 */
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { run } from './exec-helpers.js'
import {
  watchCi, parseRunState, parseCheckStates,
  formatRunTerminal, formatShaTerminal,
} from './ci-watch-core.js'

const REPO_SLUG = 'MajidAsghariTabrizi/anti-gravity-phoenix-v4'

export function ciWatchTool(governor, root) {
  return {
    name: 'phoenix_ci_watch',
    description:
      'Suspend the model and watch one GitHub Actions run (or the checks of one SHA) until a TERMINAL CI outcome: status=completed with a real conclusion (success/failure/cancelled/skipped/timed_out/action_required/neutral/stale). An already-terminal target returns IMMEDIATELY after one fresh read — repeated calls for the same run are cheap and never re-enter a long wait. Non-terminal targets poll INSIDE the tool (10s interval, zero model rounds); queued->in_progress transitions are NOT wake reasons. A completed/failure observation is a valid terminal result, not a tool failure. Fail-closed on deadline; prompt-exits on abort.',
    parameters: {
      runId: { type: 'string', description: 'GitHub Actions run id (numeric)' },
      sha: { type: 'string', description: 'alternative: watch check runs for this SHA (waits until ALL discovered check runs are terminal)' },
      maxWaitMinutes: { type: 'integer', description: 'deadline in minutes (default 30, max 120)' },
    },
    execute: async (args, exec) => {
      const sid = exec?.agent?.session?.id ?? 'default'
      const maxWaitMs = Math.min(Math.max(Number(args.maxWaitMinutes ?? 30) || 30, 1), 120) * 60000
      const runId = String(args.runId ?? '').trim()
      const sha = String(args.sha ?? '').trim()
      if (!runId && !sha) return 'error: runId or sha required'
      const waitId = `ci:${runId || sha}`
      // Governor wait bookkeeping is cleared in ONE finally block for every
      // exit path: immediate terminal, terminal-after-wait, deadline,
      // read error, abort, exception. No stale registered wait survives.
      governor.registerWait(sid, waitId, Date.now() + maxWaitMs, `ci watch ${runId || sha}`)
      let jobsLine = null
      const failedJobsLine = async () => {
        if (!runId || jobsLine !== null) return jobsLine
        try {
          const res = await run('gh', ['api', `repos/${REPO_SLUG}/actions/runs/${runId}/jobs`, '--jq', '[.jobs[] | select((.conclusion // "pending") != "success") | .name + "=" + (.conclusion // "pending")] | join("; ")'], { timeoutMs: 30000, writeArtifact: false })
          jobsLine = res.ok ? String(res.stdout ?? '').trim().slice(0, 600) : ''
        } catch { jobsLine = '' }
        return jobsLine
      }
      try {
        const readState = async () => {
          let res
          if (runId) {
            res = await run('gh', ['api', `repos/${REPO_SLUG}/actions/runs/${runId}`, '--jq', '.status + "|" + (.conclusion // "pending")'], { timeoutMs: 30000, writeArtifact: false })
          } else {
            res = await run('gh', ['api', `repos/${REPO_SLUG}/commits/${sha}/check-runs`, '--jq', '[.check_runs[] | .status + "|" + (.conclusion // "pending")] | join(";")'], { timeoutMs: 30000, writeArtifact: false })
          }
          if (!res.ok) throw new Error(`gh api failed: ${res.code}`)
          return res.stdout.trim()
        }
        const watched = await watchCi({
          read: readState,
          mode: runId ? 'run' : 'sha',
          intervalMs: 10000,
          maxWaitMs,
          signal: exec?.signal,
          failClosedMessage: `CI watch deadline reached before a terminal outcome — fail closed`,
        })
        if (!watched.ok && watched.aborted) {
          return `CI WATCH ABORTED after ${Math.round(watched.waitedMs / 1000)}s (${watched.checks} reads) — wait cleared, no further polling`
        }
        if (!watched.ok) {
          return `error: ${watched.error}${watched.lastState ? `; last state: ${watched.lastState}` : ''} (waited ${Math.round(watched.waitedMs / 1000)}s, ${watched.checks} reads — fail closed)`
        }
        const waitedSeconds = Math.round(watched.waitedMs / 1000)
        if (runId) {
          const parsed = parseRunState(watched.state)
          const lines = [formatRunTerminal(runId, parsed, waitedSeconds, watched.checks)]
          if (parsed.conclusion.toLowerCase() !== 'success') {
            const failed = await failedJobsLine()
            if (failed) lines.push(`failed_jobs=${failed}`)
          }
          return lines.join('\n')
        }
        return formatShaTerminal(sha, parseCheckStates(watched.state), waitedSeconds, watched.checks)
      } catch (err) {
        return `error: ci watch failed: ${String(err?.message ?? err)} — fail closed`
      } finally {
        governor.clearWait(sid, waitId)
      }
    },
  }
}

export function prFlowTool(root) {
  return {
    name: 'phoenix_pr_flow',
    description:
      'GitHub PR operations via gh CLI (args-array). status: current PR + checks for a branch; checks: check runs for a branch/SHA; create_draft: create a Draft PR (requires ack=yes; never merges — protected merge is canonical). Read-only except create_draft.',
    parameters: {
      action: { type: 'string', required: true, description: 'status | checks | create_draft' },
      branch: { type: 'string', description: 'head branch (default: current branch)' },
      base: { type: 'string', description: 'base branch (default: main)' },
      title: { type: 'string', description: 'PR title (create_draft)' },
      ack: { type: 'string', description: 'literal "yes" required for create_draft' },
    },
    execute: async (args) => {
      const action = String(args.action ?? '')
      if (action === 'status' || action === 'checks') {
        const branch = String(args.branch ?? '').trim()
        let target = branch
        if (!target) {
          const b = await run('git', ['-C', root, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
          target = b.ok ? b.stdout.trim() : 'main'
        }
        const res = await run('gh', ['pr', 'view', target, '--json', 'number,title,state,url,headRefName,baseRefName'], { timeoutMs: 30000, writeArtifact: false })
        if (!res.ok) return `no PR for branch ${target} (exit ${res.code}) — use action=create_draft`
        let out = `PR ${target}:\n${res.stdout}`
        const chk = await run('gh', ['pr', 'checks', target], { timeoutMs: 45000, writeArtifact: false })
        out += `\nchecks:\n${chk.ok ? chk.stdout.slice(0, 2500) : `(unavailable: ${chk.code})`}`
        return out
      }
      if (action === 'create_draft') {
        if (String(args.ack ?? '') !== 'yes') return 'error: create_draft requires ack="yes" — show the intended scope before creating'
        const branch = String(args.branch ?? '').trim()
        const base = String(args.base ?? 'main').trim()
        const title = String(args.title ?? '').trim()
        if (!branch || !title) return 'error: branch and title required'
        const res = await run('gh', ['pr', 'create', '--draft', '--base', base, '--head', branch, '--title', title, '--body', 'Draft PR created by Phoenix Harness V3. Scope and tests described in the mission evidence.'], { timeoutMs: 60000, writeArtifact: false })
        if (!res.ok) return `error: gh pr create failed (exit ${res.code}): ${res.stdout.slice(0, 800)}`
        return `DRAFT PR created:\n${res.stdout}`
      }
      return 'error: action must be status | checks | create_draft'
    },
  }
}

export function releasePreflightTool(root) {
  const GRAPH = [
    'branch from protected main', 'focused tests', 'regression test', 'secret scan',
    'diff review', 'draft PR', 'exact-head protected CI', 'protected merge',
    'exact-main CI', 'immutable build', 'exactly one Release Controller',
    'post-install hard gate', 'controlled activation',
  ]
  return {
    name: 'phoenix_release_preflight',
    description:
      'Read-only release-graph checklist for the current tree: git state vs protected main, diff secret scan (pattern-based), CI state of the exact head, and the 13-node protected provenance checklist with evidence refs. Never mutates; never substitutes for the canonical controller.',
    parameters: {},
    execute: async () => {
      const out = { capturedAt: new Date().toISOString(), checks: {} }
      const branch = await run('git', ['-C', root, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
      const sha = await run('git', ['-C', root, 'rev-parse', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
      const status = await run('git', ['-C', root, 'status', '--porcelain'], { timeoutMs: 15000, writeArtifact: false })
      out.checks['branch'] = { value: branch.ok ? branch.stdout.trim() : 'unknown' }
      out.checks['sha'] = { value: sha.ok ? sha.stdout.trim() : 'unknown' }
      out.checks['dirty'] = { value: status.ok ? status.stdout.trim().split('\n').filter(Boolean).length : 'unknown' }
      const diff = await run('git', ['-C', root, 'diff', 'origin/main...HEAD', '--stat'], { timeoutMs: 30000, writeArtifact: false })
      const diffText = diff.ok ? diff.stdout : '(diff unavailable)'
      out.checks['diffStat'] = { value: diffText.slice(0, 1200) }
      const secretHits = (diff.ok ? diff.stdout : '').split('\n').filter((l) => /(private[_ -]?key|BEGIN [A-Z ]*PRIVATE|sk_live|api[_-]?key\s*[:=]|password\s*[:=]\s*[^$'"]{4,})/i.test(l)).slice(0, 5)
      out.checks['secretScan'] = { value: secretHits.length === 0 ? 'no obvious secret patterns in diff stat' : `POSSIBLE HITS:\n${secretHits.join('\n')}` }
      const ci = await run('gh', ['api', `repos/${REPO_SLUG}/commits/${out.checks.sha.value}/check-runs`, '--jq', '[.check_runs[] | .name + "=" + .status + "/" + (.conclusion // "pending")] | join("; ")'], { timeoutMs: 30000, writeArtifact: false })
      out.checks['exactHeadCI'] = { value: ci.ok ? ci.stdout.slice(0, 1500) : `(unavailable: ${ci.code})` }
      const lines = ['RELEASE PREFLIGHT (read-only) — protected provenance checklist', `branch: ${out.checks.branch.value} | sha: ${out.checks.sha.value} | dirty: ${out.checks.dirty.value}`, `diff vs main: ${out.checks.diffStat.value}`, `secret scan: ${out.checks.secretScan.value}`, `exact-head CI: ${out.checks.exactHeadCI.value}`, '', '13-node graph (check each with evidence before any release action):', ...GRAPH.map((g, i) => `  ${String(i + 1).padStart(2)}. ${g}`)]
      return lines.join('\n')
    },
  }
}

export function releaseDispatchTool(root, missionModule) {
  const ALLOWED_SCRIPTS = {
    'release_provenance.py': join(root, 'scripts', 'release_provenance.py'),
  }
  return {
    name: 'phoenix_release_dispatch',
    description:
      'PRODUCTION MUTATION GATE. Never dispatches on its own: requires (1) current-session mission with riskTier=prod_mutation, (2) owner approval recorded in the mission, (3) ack matching the mission objective, (4) the command being one of the canonical release scripts, and (5) release_preflight gates documented. Then prints the exact MUTATION PLAN for owner confirmation; execution only of the canonical script with --dry-run semantics unless plan_ack is provided. Fail-closed otherwise. Financial execution authority is never modified.',
    parameters: {
      command: { type: 'string', required: true, description: 'canonical script name: release_provenance.py' },
      args: { type: 'array', items: { type: 'string' }, description: 'script arguments (must include --dry-run unless plan_ack=yes)' },
      ack: { type: 'string', required: true, description: 'must equal the mission objective (exact)' },
      plan_ack: { type: 'string', description: 'literal "yes" ONLY after the owner explicitly approved the printed MUTATION PLAN' },
    },
    execute: async (args, exec) => {
      const sid = exec?.agent?.session?.id ?? 'default'
      const mission = missionModule.readMission(root, sid)
      if (!mission.exists) return 'error: REFUSED — no mission exists; create a prod_mutation mission first'
      const m = mission.spec
      if (m.riskTier !== 'prod_mutation') return 'error: REFUSED — mission riskTier is not prod_mutation'
      if (!m.ownerApproval) return 'error: REFUSED — owner approval not recorded in the mission (phoenix_mission owner_approval)'
      if (String(args.ack ?? '') !== m.objective) return 'error: REFUSED — ack must equal the mission objective exactly'
      const script = ALLOWED_SCRIPTS[String(args.command ?? '')]
      if (!script) return `error: REFUSED — unknown command; allowed: ${Object.keys(ALLOWED_SCRIPTS).join(', ')}`
      if (!existsSync(script)) return 'error: REFUSED — canonical script missing from the tree'
      const scriptArgs = Array.isArray(args.args) ? args.args.map(String) : []
      const dryRun = scriptArgs.includes('--dry-run')
      const planAck = String(args.plan_ack ?? '') === 'yes'
      const plan = [
        'MUTATION PLAN (prepared; NOT executed)',
        `root cause / intent: ${m.objective}`,
        `authority: owner approval ${m.ownerApproval.approvedAt} (${m.ownerApproval.scope})`,
        `action: ${args.command} ${scriptArgs.join(' ')}`,
        'affected: Production release control plane (canonical controller path only)',
        'safety invariants: protected provenance chain, exactly one controller, post-install hard gate, rollback path',
        'rollback: canonical rollback-release.sh / controller rollback state',
        'post-state verification: phoenix_release_verify + health gate',
      ].join('\n')
      if (!dryRun && !planAck) return `${plan}\n\nREFUSED to execute: args must include --dry-run, or plan_ack="yes" after explicit owner approval of this exact plan.`
      if (planAck && !dryRun) {
        const res = await run(process.execPath, [script, ...scriptArgs], { cwd: root, timeoutMs: 15 * 60000, artifactTag: 'release-dispatch', artifactDir: 'phoenix_release_dispatch', writeArtifact: true })
        if (!res.ok) return `${plan}\n\nDISPATCH FAILED (exit ${res.code}) — fail closed:\n${res.stdout.slice(0, 2000)}`
        return `${plan}\n\nDISPATCH EXECUTED (exit 0):\n${res.stdout.slice(0, 2000)}${res.artifact ? `\nartifact: ${res.artifact}` : ''}\n\nPOST: run phoenix_release_verify and compare actual vs expected; fail-close on ambiguity.`
      }
      // dry-run
      const res = await run(process.execPath, [script, ...scriptArgs], { cwd: root, timeoutMs: 15 * 60000, artifactTag: 'release-dryrun', artifactDir: 'phoenix_release_dispatch', writeArtifact: true })
      if (!res.ok) return `${plan}\n\nDRY-RUN FAILED (exit ${res.code}):\n${res.stdout.slice(0, 2000)}`
      return `${plan}\n\nDRY-RUN OK:\n${res.stdout.slice(0, 2000)}${res.artifact ? `\nartifact: ${res.artifact}` : ''}`
    },
  }
}
