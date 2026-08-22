#!/usr/bin/env node
// Phoenix Harness V3 — regenerable source provenance manifest.
// Computes SHA-256 per file over the canonical committed tree and an
// aggregate tree hash over the sorted "relpath|sha256" lines.
//
// Usage:
//   node tools/phoenix-harness-v3/src/preflight/source-manifest.mjs
//   node tools/phoenix-harness-v3/src/preflight/source-manifest.mjs <rootDir> <outJson>
//
// Skips machine-local artifact trees (same policy as the committed .gitignore):
//   .phoenix-harness/, benchmarks/frontier/runs/, node_modules/, dist/, build/.
import { createHash } from 'node:crypto';
import { readdirSync, statSync, readFileSync, writeFileSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

const cwd = process.cwd();
const root = resolve(process.argv[2] ?? join(cwd, 'tools', 'phoenix-harness-v3'));
const out = process.argv[3] ?? join(root, 'reports', 'source-manifest.json');

const SKIP_DIRS = new Set(['.phoenix-harness', 'node_modules', 'dist', 'build', 'runs']);

function walk(dir, acc) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (SKIP_DIRS.has(entry.name)) continue;
      walk(full, acc);
    } else if (entry.isFile()) {
      acc.push(full);
    }
  }
  return acc;
}

const files = walk(root, []).sort();
const hashes = [];
const byPath = {};
for (const file of files) {
  const rel = relative(root, file).replaceAll('\\', '/');
  const sha = createHash('sha256').update(readFileSync(file)).digest('hex');
  byPath[rel] = sha;
  hashes.push(`${rel}|${sha}`);
}
const treeHash = createHash('sha256').update(hashes.join('\n')).digest('hex');

const manifest = {
  generatedAt: new Date().toISOString(),
  root: relative(cwd, root).replaceAll('\\', '/'),
  fileCount: files.length,
  treeHash,
  files: byPath,
};
writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
console.log(`manifest: ${files.length} files -> ${relative(cwd, out)} (tree ${treeHash})`);
