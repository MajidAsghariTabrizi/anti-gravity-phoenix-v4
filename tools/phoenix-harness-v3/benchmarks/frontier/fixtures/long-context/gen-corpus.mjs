#!/usr/bin/env node
/**
 * Deterministic corpus generator for the long-context frontier task.
 * Writes 64 markdown files (~320K chars total) under
 * benchmarks/frontier/fixtures/long-context/corpus/ plus a fact key at
 * benchmarks/frontier/fixtures/long-context/questions.json.
 *
 * Deterministic: fixed seed, no timestamps, no machine paths. The runner
 * regenerates it in every temp worktree so corpus bytes are identical
 * across arms and runs.
 */
import { writeFileSync, mkdirSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = join(dirname(fileURLToPath(import.meta.url)))
const CORPUS = join(ROOT, 'corpus')

// LCG with fixed seed
let s = 0x5EED_2026 >>> 0
function rnd() {
  s = (Math.imul(s, 1664525) + 1013904223) >>> 0
  return s / 0xFFFFFFFF
}
function pick(arr) { return arr[Math.floor(rnd() * arr.length)] }
function int(n) { return Math.floor(rnd() * n) }

const UNITS = ['bridge', 'sequencer', 'relay', 'feed', 'observer', 'gateway', 'recorder', 'engine']
const ACTIONS = ['validates', 'aggregates', 'deduplicates', 'ratifies', 'drains', 'queues', 'escrows', 'snapshots']
const ADJ = ['stale', 'finalized', 'shadow', 'reconciled', 'pending', 'armored', 'bounded', 'canonical']
const NOUNS = ['batch', 'window', 'nonce', 'epoch', 'receipt', 'lane', 'route', 'slot']

function section(id, fileIdx, secIdx) {
  const factId = `F-${String(fileIdx).padStart(3, '0')}-${secIdx}`
  const artifact = `artifact-${pick(UNITS)}-${int(90000) + 10000}`
  const qty = int(90) + 10
  const desc = `${pick(ADJ)} ${pick(NOUNS)} ${pick(ACTIONS)} ${qty} items`
  const pad = Array.from({ length: 4 }, () =>
    `${pick(UNITS)} ${pick(NOUNS)} ${pick(ADJ)} ${pick(ACTIONS)} ${int(900) + 100} ${pick(NOUNS)}s in epoch ${int(999) + 1}; ` +
    `bound ${int(300) + 50} ${pick(NOUNS)}s, lane ${pick(['aave', 'atlas', 'generic-dex'])} ${pick(['closed', 'shadow', 'read-only'])}.`).join('\n')
  return `## ${secIdx}. ${pick(UNITS)} ${pick(NOUNS)} ${pick(ADJ)}\n\n` +
    `Fact id \`${factId}\`: ${artifact} — ${desc}. Revision ${int(9) + 1}. ` +
    `Owner lane ${pick(['aave', 'atlas', 'generic-dex'])} (${pick(['closed', 'shadow', 'read-only'])}). ` +
    `Bounded by ${int(300) + 50} units per ${pick(['minute', 'hour', 'epoch'])}.\n\n${pad}\n`
}

function file(idx) {
  const body = []
  for (let sIdx = 1; sIdx <= 12; sIdx++) body.push(section(idx, idx, sIdx))
  return `# Corpus ${String(idx).padStart(3, '0')} — ${pick(UNITS)} domain\n\n` +
    `Deterministic evaluation corpus. Machine-generated; never edited by agents.\n\n` +
    body.join('\n')
}

const questions = []
for (let i = 1; i <= 64; i++) {
  const qs = new Set()
  while (qs.size < 2) qs.add(int(12) + 1)
  for (const sec of qs) {
    const factId = `F-${String(i).padStart(3, '0')}-${sec}`
    questions.push({
      id: `Q-${String(i).padStart(3, '0')}-${sec}`,
      corpusFile: `corpus-${String(i).padStart(3, '0')}.md`,
      section: sec,
      factId,
      ask: `In corpus file corpus-${String(i).padStart(3, '0')}.md section ${sec}: what is the exact fact id, artifact identifier, and the one-line description?`,
    })
  }
}

mkdirSync(CORPUS, { recursive: true })
for (let i = 1; i <= 64; i++) {
  writeFileSync(join(CORPUS, `corpus-${String(i).padStart(3, '0')}.md`), file(i), 'utf8')
}
writeFileSync(join(ROOT, 'questions.json'), JSON.stringify({ generatedAt: 'deterministic', questions }, null, 2))
const total = 64 * 8
console.log(`corpus: 64 files, ${questions.length}/${total} questions -> ${CORPUS}`)
console.log(`questions key: ${join(ROOT, 'questions.json')}`)
