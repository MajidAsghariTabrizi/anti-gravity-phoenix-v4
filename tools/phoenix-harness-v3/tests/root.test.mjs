/**
 * Canonical Phoenix repo-root resolution tests (V3 quickfix — repo-root bug).
 * Verifies precedence env > configured > marker discovery, ancestor walk,
 * and the never-undefined fallback (fail-open on resolution, fail-closed
 * on the individual git/knowledge operations).
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const { resolvePhoenixRepoRoot, discoverPhoenixRepo, looksLikePhoenixRepo, REPO_MARKERS } = await import(pathToFileURL(join(ROOT, 'src', 'root.js')).href)

function makeRepoTree() {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-root-'))
  const repo = join(tmp, 'a', 'b', 'repo')
  mkdirSync(join(repo, '.git'), { recursive: true })
  mkdirSync(join(repo, '.phoenix-harness'), { recursive: true })
  mkdirSync(join(repo, 'src', 'deep'), { recursive: true })
  return { tmp, repo }
}

test('env PHOENIX_REPO_ROOT wins over configured and discovery', () => {
  const { tmp, repo } = makeRepoTree()
  try {
    const other = join(tmp, 'env-root')
    mkdirSync(join(other, '.git'), { recursive: true })
    mkdirSync(join(other, '.phoenix-harness'), { recursive: true })
    const out = resolvePhoenixRepoRoot(repo, { cwd: repo, env: { PHOENIX_REPO_ROOT: other } })
    assert.equal(out, resolve(other))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('configured workspaceRoot wins over cwd discovery when it looks like the repo', () => {
  const { tmp, repo } = makeRepoTree()
  try {
    const configured = join(tmp, 'configured')
    mkdirSync(join(configured, '.git'), { recursive: true })
    mkdirSync(join(configured, '.phoenix-harness'), { recursive: true })
    const out = resolvePhoenixRepoRoot(configured, { cwd: repo, env: {} })
    assert.equal(out, resolve(configured))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('discovery walks cwd ancestors to the marker-bearing root', () => {
  const { tmp, repo } = makeRepoTree()
  try {
    const deep = join(repo, 'src', 'deep')
    assert.equal(discoverPhoenixRepo(deep), repo)
    assert.equal(resolvePhoenixRepoRoot('', { cwd: deep, env: {} }), repo)
    assert.equal(resolvePhoenixRepoRoot(null, { cwd: deep, env: {} }), repo)
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('invalid configured/env candidates fall through to discovery, then expected, then cwd', () => {
  const { tmp, repo } = makeRepoTree()
  try {
    const bogus = join(tmp, 'bogus')
    mkdirSync(bogus, { recursive: true })
    // configured path does not look like the repo -> discovery wins
    assert.equal(resolvePhoenixRepoRoot(bogus, { cwd: repo, env: {} }), repo)
    // nothing matches anywhere -> last resorts (documented path, then cwd);
    // a non-matching expected path disables the documented-path fallback
    const nowhere = join(tmp, 'nowhere')
    mkdirSync(nowhere, { recursive: true })
    assert.equal(resolvePhoenixRepoRoot('', { cwd: nowhere, env: {}, expected: join(tmp, 'not-the-repo') }), resolve(nowhere))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('looksLikePhoenixRepo requires every marker', () => {
  const { tmp, repo } = makeRepoTree()
  try {
    assert.ok(looksLikePhoenixRepo(repo))
    const half = join(tmp, 'half')
    mkdirSync(join(half, '.git'), { recursive: true })
    assert.ok(!looksLikePhoenixRepo(half))
    assert.deepEqual(REPO_MARKERS, ['.git', '.phoenix-harness'])
    assert.ok(!looksLikePhoenixRepo(null))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})
