/**
 * Canonical Phoenix repository root resolution (V3 quickfix — repo-root bug).
 *
 * Phoenix V3 tools must NEVER assume process.cwd() is the Phoenix repo: the
 * harness mounts the preset with the process cwd at boot, and sessions may
 * start anywhere. One canonical PHOENIX_REPO_ROOT is resolved here and used
 * consistently by phoenix_repo_snapshot, phoenix_current_truth,
 * phoenix_context, phoenix_ground_truth, phoenix_opportunity_replay, the
 * knowledge/evidence paths, and git SHA/branch detection.
 *
 * Precedence (configured/discovered first, hard-coded only as last resort):
 *   1. PHOENIX_REPO_ROOT environment variable (explicit operator config)
 *   2. an explicitly configured workspaceRoot (composition config)
 *   3. discovery: process.cwd() and its ancestors containing BOTH a .git
 *      directory and a .phoenix-harness directory (the Phoenix repo markers)
 *   4. the documented deployment path (only if it exists and looks like the
 *      Phoenix repo)
 *   5. process.cwd() — never undefined, so tools keep working (fail-open on
 *      resolution, fail-closed on the individual git/knowledge operations).
 */
import { existsSync } from 'node:fs'
import { resolve, dirname, parse } from 'node:path'

/** Documented deployment location — last-resort fallback, not an assumption. */
export const EXPECTED_REPO_ROOT =
  'C:\\Users\\ma.asghari\\PycharmProjects\\PhoenixAgent\\anti-gravity-phoenix-v4'

export const REPO_MARKERS = ['.git', '.phoenix-harness']

/** A directory qualifies as the Phoenix repo when all markers exist. */
export function looksLikePhoenixRepo(dir, markers = REPO_MARKERS) {
  if (!dir) return false
  try {
    return markers.every((m) => existsSync(resolve(dir, m)))
  } catch {
    return false
  }
}

/** Walk `start` and its ancestors; return the first dir with all markers. */
export function discoverPhoenixRepo(start, markers = REPO_MARKERS) {
  let cur = resolve(start)
  for (;;) {
    if (looksLikePhoenixRepo(cur, markers)) return cur
    const parent = dirname(cur)
    if (parent === cur) return null
    const { root } = parse(cur)
    if (cur === root && parent === cur) return null
    cur = parent
  }
}

/**
 * Resolve the one canonical Phoenix repo root.
 *
 * @param {string} [configured]   workspaceRoot from composition config
 *                                ('' / null means "not configured").
 * @param {{cwd?: string, env?: Record<string,string|undefined>,
 *          expected?: string, markers?: string[]}} [opts]
 * @returns {string} absolute path; never throws.
 */
export function resolvePhoenixRepoRoot(configured, opts = {}) {
  const env = opts.env ?? process.env
  const cwd = opts.cwd ?? process.cwd()
  const expected = opts.expected ?? EXPECTED_REPO_ROOT
  const markers = opts.markers ?? REPO_MARKERS
  const candidates = []

  const envRoot = env?.PHOENIX_REPO_ROOT
  if (typeof envRoot === 'string' && envRoot.trim() !== '') {
    candidates.push({ value: envRoot.trim(), label: 'PHOENIX_REPO_ROOT env' })
  }
  if (typeof configured === 'string' && configured.trim() !== '') {
    candidates.push({ value: configured.trim(), label: 'configured workspaceRoot' })
  }
  // Discovery from the process cwd upward (covers "normal new session" starts).
  const discovered = discoverPhoenixRepo(cwd, markers)
  if (discovered) candidates.push({ value: discovered, label: 'discovered (cwd ancestors)' })

  for (const c of candidates) {
    if (looksLikePhoenixRepo(c.value, markers)) return resolve(c.value)
  }
  // Last resorts: the documented deployment path (when it really is the
  // Phoenix repo), then the cwd itself so tools degrade instead of crashing.
  if (expected && looksLikePhoenixRepo(expected, markers)) return resolve(expected)
  return resolve(cwd)
}
