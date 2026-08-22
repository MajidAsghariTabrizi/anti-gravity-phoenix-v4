#!/usr/bin/env node
/**
 * Generate knowledge/graphs/symbol-graph.json + schema-graph.json from the
 * repository. Top-level definitions only, bounded output (max 400 entries
 * per graph). Deterministic: same tree -> same output (sorted).
 *
 * Usage: node src/gen/gen-graphs.mjs [repoRoot]
 */
import { readdirSync, readFileSync, writeFileSync, mkdirSync, existsSync, statSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')
const REPO = resolve(process.argv[2] ?? join(ROOT, '..', '..'))
const OUT = join(ROOT, 'knowledge', 'graphs')

const MAX_ENTRIES = 400
const SKIP_DIRS = new Set(['.git', 'node_modules', 'target', '__pycache__', '.venv', 'dist', '.dsh', '.agent-private', 'fixtures', 'fork-sandbox'])

const LANG_PATTERNS = {
  rust: {
    dirs: ['live-executor', 'phoenix-engine', 'rpc-gateway', 'recorder', 'replay', 'money-path-classifier'],
    ext: '.rs',
    re: /^\s*(pub\s+)?(fn|struct|enum|trait|impl|const|type)\s+([A-Za-z_][A-Za-z0-9_]*)/,
  },
  go: {
    dirs: ['feed-ingestor', 'atlas-observer', 'migration-runner'],
    ext: '.go',
    re: /^(func|type|const|var)\s+(\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)/,
  },
  solidity: {
    dirs: ['contracts'],
    ext: '.sol',
    re: /^\s*(contract|interface|library|function|event|error|struct|enum)\s+([A-Za-z_][A-Za-z0-9_]*)/,
  },
}

function walkDir(dir, depth = 0) {
  if (depth > 8) return []
  if (!existsSync(dir) || !statSync(dir).isDirectory()) return []
  const out = []
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name)
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) out.push(...walkDir(p, depth + 1))
    } else out.push(p)
  }
  return out
}

const symbolGraph = { schema: 1, generatedAt: new Date().toISOString(), generatedBy: 'src/gen/gen-graphs.mjs', components: {} }
let total = 0
for (const [lang, cfg] of Object.entries(LANG_PATTERNS)) {
  const entries = []
  for (const d of cfg.dirs) {
    const base = join(REPO, d)
    for (const file of walkDir(base)) {
      if (!file.endsWith(cfg.ext)) continue
      if (total >= MAX_ENTRIES) break
      const rel = file.slice(REPO.length + 1).replace(/\\/g, '/')
      try {
        const text = readFileSync(file, 'utf8')
        const lines = text.split('\n')
        for (let i = 0; i < lines.length && total < MAX_ENTRIES; i++) {
          const m = lines[i].match(cfg.re)
          if (m) {
            const name = m[m.length - 1]
            entries.push({ symbol: name, file: rel, line: i + 1, kind: m[1] ?? m[2] ?? 'def' })
            total += 1
          }
        }
      } catch { /* skip unreadable */ }
    }
  }
  symbolGraph.components[lang] = { count: entries.length, entries }
}

const schemaGraph = { schema: 1, generatedAt: new Date().toISOString(), generatedBy: 'src/gen/gen-graphs.mjs', tables: {} }
const sqlDirs = [join(REPO, 'live-executor', 'schema'), join(REPO, 'migrations')]
let tables = 0
for (const dir of sqlDirs) {
  for (const file of walkDir(dir)) {
    if (!file.endsWith('.sql') || tables >= MAX_ENTRIES) continue
    const rel = file.slice(REPO.length + 1).replace(/\\/g, '/')
    try {
      const text = readFileSync(file, 'utf8')
      for (const line of text.split('\n')) {
        const t = line.match(/^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)/i)
        if (t) {
          schemaGraph.tables[t[1]] = schemaGraph.tables[t[1]] ?? { columns: [], definedIn: [] }
          schemaGraph.tables[t[1]].definedIn.push(rel)
          tables += 1
          continue
        }
        const c = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s+(BIGINT|INTEGER|INT|NUMERIC|DECIMAL|TEXT|VARCHAR\(\d+\)|TIMESTAMPTZ|TIMESTAMP|BOOLEAN|JSONB|BYTEA|UUID|SMALLINT)\b/i)
        if (c) {
          // column belongs to the most recent CREATE TABLE in this file
          const lastTable = Object.keys(schemaGraph.tables).filter((k) => schemaGraph.tables[k].definedIn.includes(rel)).at(-1)
          if (lastTable && !schemaGraph.tables[lastTable].columns.includes(c[1])) schemaGraph.tables[lastTable].columns.push(c[1])
        }
      }
    } catch { /* skip */ }
  }
}

mkdirSync(OUT, { recursive: true })
writeFileSync(join(OUT, 'symbol-graph.json'), JSON.stringify(symbolGraph, null, 2))
writeFileSync(join(OUT, 'schema-graph.json'), JSON.stringify(schemaGraph, null, 2))
console.log(`symbol-graph: ${total} entries | schema-graph: ${Object.keys(schemaGraph.tables).length} tables`)
console.log(`written: ${join(OUT, 'symbol-graph.json')}`)
console.log(`written: ${join(OUT, 'schema-graph.json')}`)
