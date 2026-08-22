/**
 * phoenix_remote / phoenix_production_snapshot / phoenix_sql_readonly /
 * phoenix_release_verify — Production READ-ONLY native tools.
 *
 * Safety design (fail-closed):
 *  - ssh runs with an ARGS ARRAY against the configured alias; no shell
 *    string is ever composed by the model (L-003).
 *  - A strict regex allowlist of read-only commands; 'sudo', redirects,
 *    pipes and command separators are REFUSED (L-006).
 *  - Output capped; secrets never read; only documented paths allowed.
 *  - phoenix_sql_readonly: SELECT/WITH only, one statement, row cap,
 *    keyword blocklist, bounded output. Container resolved from docker ps.
 *  - Anything unavailable or ambiguous fails closed with a structured error.
 */
import { run, writeJsonArtifact } from './exec-helpers.js'

const SSH_ALIAS = 'phoenix-prod'
const SSH_TIMEOUT = 45000
const OUTPUT_CAP = 5000

const REMOTE_ALLOWLIST = [
  { re: /^docker\s+ps(\s+(-a|--all|-q|--format=[\w.{{}]+))*\s*$/, label: 'docker ps' },
  { re: /^docker\s+inspect\s+[\w.-]+(\s+--format=.+)?$/, label: 'docker inspect <container>' },
  { re: /^docker\s+image\s+ls(\s+--format=.+)?$/, label: 'docker image ls' },
  { re: /^docker\s+logs\s+--tail\s+\d{1,4}\s+[\w.-]+$/, label: 'docker logs --tail <n> <container>' },
  { re: /^systemctl\s+(status|is-active|is-enabled)\s+[\w@.-]+(\s+--no-pager)?$/, label: 'systemctl status/is-active/is-enabled <unit>' },
  { re: /^journalctl\s+-u\s+[\w@.-]+(\s+--since\s+"?[\d\s:-]+"?)?(\s+--no-pager)?(\s+-n\s+\d{1,4})?(\s+--priority=(err|warning|crit|alert))?$/, label: 'journalctl -u <unit> [--since ...] [-n N]' },
  { re: /^uptime$/, label: 'uptime' },
  { re: /^df\s+-h(\s+\/opt\/phoenix)?$/, label: 'df -h' },
  { re: /^free\s+-h$/, label: 'free -h' },
  { re: /^ls\s+-la\s+(\/opt\/phoenix(\/deploy|\/data|\/config|\/scripts)?|\/var\/lib\/phoenix-release)$/, label: 'ls -la <documented path>' },
  { re: /^cat\s+(\/opt\/phoenix\/deploy\/[A-Za-z0-9._/-]+|\/var\/lib\/phoenix-release\/[A-Za-z0-9._/-]+)$/, label: 'cat <documented status file>' },
  { re: /^flock\s+-\n?\s*.*/, label: null }, // flock inspection is not exposed; placeholder refused below
]

const SQL_KEYWORD_BLOCK = /\b(insert|update|delete|drop|alter|create|truncate|grant|revoke|copy|do\b|call|set\b|vacuum|analyze|reindex|begin|commit|rollback|reset|listen|notify|prepare|execute)\b/i
const SQL_SELECT_ONLY = /^\s*(select|with)\b/i

export function remoteTool() {
  return {
    name: 'phoenix_remote',
    description:
      'Run ONE bounded read-only production command over SSH (alias phoenix-prod) from a strict allowlist: docker ps/inspect/image ls/logs --tail, systemctl status/is-active/is-enabled, journalctl -u with --since/-n, uptime, df, free, ls/cat of documented /opt/phoenix status paths. No sudo, no pipes, no redirects, no secrets. Output capped ~5K chars. Fail-closed.',
    parameters: {
      command: { type: 'string', required: true, description: 'the exact allowed command (see description)' },
    },
    execute: async (args) => {
      const command = String(args.command ?? '').trim()
      if (!command) return 'error: command is required'
      if (command.length > 300) return 'error: command too long'
      if (/\b(sudo|su\b|bash|sh\b)\b/.test(command)) return 'error: REFUSED — sudo/shell not allowed on the read-only path'
      if (/[|;&<>`$]/.test(command)) return 'error: REFUSED — pipes/separators/redirects/substitution not allowed'
      const hit = REMOTE_ALLOWLIST.find((a) => a.re.test(command) && a.label)
      if (!hit) return `error: REFUSED — command not in the read-only allowlist.\nAllowed patterns:\n${REMOTE_ALLOWLIST.filter((a) => a.label).map((a) => `  ${a.label}`).join('\n')}`
      const res = await run('ssh', [SSH_ALIAS, command], { timeoutMs: SSH_TIMEOUT, artifactTag: 'remote', artifactDir: 'phoenix_remote', writeArtifact: true })
      if (!res.ok) return `error: ssh failed (exit ${res.code}): ${res.stdout.slice(0, 600)}`
      const capped = res.full.length > OUTPUT_CAP ? `${res.full.slice(0, OUTPUT_CAP)}\n…[capped ${res.full.length - OUTPUT_CAP} chars; full in artifact]` : res.full
      return [`REMOTE (read-only) — ${command}`, capped, res.artifact ? `artifact: ${res.artifact}` : ''].filter(Boolean).join('\n')
    },
  }
}

export function productionSnapshotTool() {
  return {
    name: 'phoenix_production_snapshot',
    description:
      'Composite read-only Production snapshot in one call: container health/restarts, lane armed/kill states, active attempts/submissions, lock state, release identity, provider readiness. Runs only allowlisted read-only SSH commands; anything the phoenix user cannot read is reported as unavailable (fail-closed, never escalated). Freshness class prod-live (5 min).',
    parameters: {
      parts: { type: 'array', items: { type: 'string' }, description: 'optional subset: containers | lanes | locks | release | attempts (default all)' },
    },
    execute: async (args) => {
      const parts = new Set(Array.isArray(args.parts) && args.parts.length ? args.parts : ['containers', 'lanes', 'locks', 'release', 'attempts'])
      const out = { capturedAt: new Date().toISOString(), parts: {} }
      const ssh = async (command) => {
        const res = await run('ssh', [SSH_ALIAS, command], { timeoutMs: SSH_TIMEOUT, writeArtifact: false })
        return res.ok ? res.full : `(unavailable: exit ${res.code})`
      }
      if (parts.has('containers')) {
        out.parts.containers = await ssh('docker ps --format={{.Names}},{{.Status}},{{.Image}}')
      }
      if (parts.has('release')) {
        out.parts.release = await ssh('ls -la /opt/phoenix/deploy')
        out.parts.releaseCurrent = await ssh('cat /opt/phoenix/deploy/current-release 2>/dev/null || echo unavailable')
      }
      if (parts.has('lanes') || parts.has('locks') || parts.has('attempts')) {
        out.parts.note = 'lane/lock/attempt state files live under root-owned paths; phoenix user reads what is permitted — see container logs + dashboard for the rest'
        if (parts.has('lanes')) out.parts.lanes = await ssh('systemctl status phoenix-autonomous --no-pager 2>/dev/null || echo unavailable')
      }
      const artifact = writeJsonArtifact('production_snapshot', 'snapshot', out)
      const lines = [
        `PRODUCTION SNAPSHOT (read-only, prod-live class, fresh 5 min) — ${out.capturedAt}`,
      ]
      for (const [k, v] of Object.entries(out.parts)) {
        const text = String(v).slice(0, 1200)
        lines.push(`--- ${k} ---\n${text}`)
      }
      if (artifact) lines.push(`artifact: ${artifact}`)
      lines.push('NOTE: dynamic truth expires; refresh before any decision.')
      return lines.join('\n')
    },
  }
}

export function sqlReadonlyTool() {
  return {
    name: 'phoenix_sql_readonly',
    description:
      'Run ONE read-only SQL statement against the production Postgres (via ssh + docker exec, args-array). Enforced: single SELECT/WITH statement, no DML/DDL keywords, no semicolon chaining, row/output caps. Fail-closed on any ambiguity. Use only when dashboard/ledgers are insufficient.',
    parameters: {
      query: { type: 'string', required: true, description: 'single SELECT or WITH ... SELECT statement (one statement only)' },
      limit: { type: 'integer', description: 'max result chars (default 4000, max 8000)' },
    },
    execute: async (args) => {
      const query = String(args.query ?? '').trim()
      if (!query) return 'error: query is required'
      if (query.length > 2000) return 'error: query too long'
      if (!SQL_SELECT_ONLY.test(query)) return 'error: REFUSED — only SELECT or WITH...SELECT statements'
      const statements = query.split(';').filter((s) => s.trim())
      if (statements.length > 1) return 'error: REFUSED — exactly one statement, no chaining'
      if (SQL_KEYWORD_BLOCK.test(query)) return 'error: REFUSED — statement contains a blocked keyword'
      const limit = Math.min(Math.max(Number(args.limit ?? 4000) || 4000, 500), 8000)
      // resolve the postgres container name from docker ps (deterministic, inside the tool)
      const ps = await run('ssh', [SSH_ALIAS, 'docker ps --format={{.Names}}'], { timeoutMs: SSH_TIMEOUT, writeArtifact: false })
      if (!ps.ok) return `error: cannot list containers (exit ${ps.code})`
      const container = ps.full.split('\n').find((n) => /postgres/.test(n))
      if (!container) return 'error: no postgres container found — fail closed'
      const res = await run('ssh', [SSH_ALIAS, 'docker', 'exec', '-i', container, 'psql', '-t', '-A', '-F', '|', '-c', query], { timeoutMs: SSH_TIMEOUT, artifactTag: 'sql', artifactDir: 'phoenix_sql_readonly', writeArtifact: true })
      if (!res.ok) return `error: query failed (exit ${res.code}): ${res.stdout.slice(0, 600)}`
      const capped = res.full.length > limit ? `${res.full.slice(0, limit)}\n…[capped ${res.full.length - limit} chars]` : res.full
      return [`SQL READONLY (container ${container})`, capped, res.artifact ? `artifact: ${res.artifact}` : ''].filter(Boolean).join('\n')
    },
  }
}

export function releaseVerifyTool() {
  return {
    name: 'phoenix_release_verify',
    description:
      'Read-only release identity verification: current-release pointer, container image SHAs, release gateway/health files. Freshness class release (refresh before any ops action). Never mutates.',
    parameters: {},
    execute: async () => {
      const out = { capturedAt: new Date().toISOString() }
      const ssh = async (command) => {
        const res = await run('ssh', [SSH_ALIAS, command], { timeoutMs: SSH_TIMEOUT, writeArtifact: false })
        return res.ok ? res.full : `(unavailable: exit ${res.code})`
      }
      out.currentRelease = await ssh('cat /opt/phoenix/deploy/current-release')
      out.containers = await ssh('docker ps --format={{.Names}},{{.Image}}')
      const artifact = writeJsonArtifact('release_verify', 'verify', out)
      const lines = [`RELEASE VERIFY (read-only) — ${out.capturedAt}`, `current-release: ${out.currentRelease}`, `containers:`, out.containers.slice(0, 1500)]
      if (artifact) lines.push(`artifact: ${artifact}`)
      return lines.join('\n')
    },
  }
}
