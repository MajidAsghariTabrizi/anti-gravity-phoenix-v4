/**
 * phoenix_repo_snapshot + phoenix_symbol — local repository truth.
 * Read-only, bounded, args-array spawns only.
 */
import { readdirSync, existsSync, statSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { run, writeJsonArtifact } from './exec-helpers.js'

const REPO_DIRS = ['live-executor', 'phoenix-engine', 'rpc-gateway', 'recorder', 'replay', 'money-path-classifier', 'fork-sandbox', 'feed-ingestor', 'atlas-observer', 'migration-runner', 'dashboard', 'contracts', 'migrations', 'live-executor/schema', 'docs', 'scripts', 'config']

export function repoSnapshotTool(workspaceRoot) {
  return {
    name: 'phoenix_repo_snapshot',
    description:
      'Snapshot local repository truth in one bounded call: git branch/SHA/status (porcelain, capped), top-level file inventory, and a focused dir listing. Read-only. Returns compact text + JSON artifact reference. Use before push/PR decisions instead of multiple git/pwsh calls.',
    parameters: {
      scope: { type: 'string', description: 'git | files | all (default all)' },
      dirs: { type: 'array', items: { type: 'string' }, description: 'optional dirs to inventory (default: key dirs)' },
    },
    execute: async (args) => {
      const out = { generatedAt: new Date().toISOString(), scope: args.scope ?? 'all', git: null, files: null }
      if (args.scope === 'all' || args.scope === 'git') {
        const branch = await run('git', ['-C', workspaceRoot, 'rev-parse', '--abbrev-ref', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
        const sha = await run('git', ['-C', workspaceRoot, 'rev-parse', 'HEAD'], { timeoutMs: 15000, writeArtifact: false })
        const status = await run('git', ['-C', workspaceRoot, 'status', '--porcelain'], { timeoutMs: 20000, writeArtifact: false })
        const remote = await run('git', ['-C', workspaceRoot, 'remote', 'get-url', 'origin'], { timeoutMs: 15000, writeArtifact: false })
        out.git = {
          branch: branch.ok ? branch.stdout.split('\n')[0] : `error:${branch.code}`,
          sha: sha.ok ? sha.stdout.split('\n')[0] : `error:${sha.code}`,
          remote: remote.ok ? remote.stdout.split('\n')[0] : `error:${remote.code}`,
          statusLines: status.ok ? status.stdout.split('\n').filter(Boolean).slice(0, 40) : [`error:${status.code}`],
          dirty: status.ok && status.stdout.trim().length > 0,
        }
      }
      if (args.scope === 'all' || args.scope === 'files') {
        const dirs = (Array.isArray(args.dirs) && args.dirs.length ? args.dirs : REPO_DIRS).map((d) => String(d)).slice(0, 20)
        out.files = {}
        for (const d of dirs) {
          const p = join(workspaceRoot, ...d.split('/'))
          try {
            if (!existsSync(p) || !statSync(p).isDirectory()) { out.files[d] = { error: 'missing or not a dir' }; continue }
            const names = readdirSync(p).slice(0, 25)
            out.files[d] = { entries: names, truncated: names.length >= 25 }
          } catch (err) {
            out.files[d] = { error: String(err?.message ?? err) }
          }
        }
      }
      const artifact = writeJsonArtifact('repo_snapshot', 'snapshot', out)
      const lines = ['REPO SNAPSHOT (read-only)', `generatedAt: ${out.generatedAt}`]
      if (out.git) {
        lines.push(`branch: ${out.git.branch}`, `sha: ${out.git.sha}`, `remote: ${out.git.remote}`, `dirty: ${out.git.dirty} (${out.git.statusLines.length} lines, capped 40)`)
        if (out.git.dirty) lines.push(...out.git.statusLines.map((s) => `  ${s}`))
      }
      if (out.files) {
        lines.push('inventory:')
        for (const [d, v] of Object.entries(out.files)) {
          lines.push(v.error ? `  ${d}: ${v.error}` : `  ${d}: ${v.entries.join(', ')}${v.truncated ? ' …(capped)' : ''}`)
        }
      }
      if (artifact) lines.push(`artifact: ${artifact}`)
      return lines.join('\n')
    },
  }
}

export function symbolTool(workspaceRoot) {
  const SEARCH_EXTS = new Set(['.rs', '.go', '.sol', '.py', '.ts', '.js', '.mjs', '.cjs', '.sql', '.yml', '.yaml', '.toml', '.json', '.md', '.sh', '.proto'])
  return {
    name: 'phoenix_symbol',
    description:
      'Locate a symbol (fn/struct/type/const/table/config key) across the repository in one bounded call. Returns up to 40 file:line hits with the matching line text. Replaces repeated grep+read rounds for symbol location. Read-only.',
    parameters: {
      symbol: { type: 'string', required: true, description: 'exact identifier or regex fragment (word-boundary match)' },
      dirs: { type: 'array', items: { type: 'string' }, description: 'optional dirs to restrict the search (default: whole repo, skipping .git/node_modules/target)' },
      limit: { type: 'integer', description: 'max hits (default 40, max 100)' },
    },
    execute: async (args) => {
      const symbol = String(args.symbol ?? '').trim()
      if (!symbol) return 'error: symbol is required'
      if (symbol.length > 200) return 'error: symbol too long'
      const limit = Math.min(Number(args.limit ?? 40) || 40, 100)
      const dirs = (Array.isArray(args.dirs) && args.dirs.length ? args.dirs : ['.']).map((d) => String(d)).slice(0, 8)
      const hits = []
      const walk = (base, depth) => {
        if (hits.length >= limit || depth > 8) return
        let entries
        try { entries = readdirSync(base, { withFileTypes: true }) } catch { return }
        for (const e of entries) {
          if (hits.length >= limit) return
          if (e.isDirectory()) {
            if (['.git', 'node_modules', 'target', '__pycache__', '.venv', 'dist', '.dsh'].includes(e.name)) continue
            walk(join(base, e.name), depth + 1)
          } else if (SEARCH_EXTS.has(e.name.slice(e.name.lastIndexOf('.')))) {
            try {
              const p = join(base, e.name)
              const text = readFileSync(p, 'utf8')
              const re = new RegExp(`(^|[^\\w])${escapeRe(symbol)}($|[^\\w])`, 'i')
              const lines = text.split('\n')
              for (let i = 0; i < lines.length && hits.length < limit; i++) {
                if (re.test(lines[i])) hits.push({ file: p.replace(workspaceRoot + '\\', '').replace(/\\/g, '/'), line: i + 1, text: lines[i].trim().slice(0, 140) })
              }
            } catch { /* skip unreadable */ }
          }
        }
      }
      for (const d of dirs) {
        const abs = join(workspaceRoot, ...d.split('/'))
        if (!existsSync(abs)) { hits.push({ file: d, line: 0, text: '(dir missing)' }); continue }
        if (statSync(abs).isDirectory()) walk(abs, 0)
        else hits.push({ file: d, line: 0, text: '(not a dir)' })
      }
      if (hits.length === 0) return `no hits for "${symbol}"`
      const artifact = writeJsonArtifact('symbol', symbol.slice(0, 40), { symbol, hits })
      const lines = [`SYMBOL "${symbol}" — ${hits.length} hits (capped ${limit})`, ...hits.map((h) => `${h.file}:${h.line}: ${h.text}`)]
      if (artifact) lines.push(`artifact: ${artifact}`)
      return lines.join('\n')
    },
  }
}

function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
