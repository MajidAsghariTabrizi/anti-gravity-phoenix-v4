#!/usr/bin/env node
/**
 * Phoenix Harness V3 eval — one-shot per-task child wrapper.
 *
 * Boots the pinned DSH checkout with a composed host tree:
 *   - dsh-base's cordis.patch.yml as a patch layer (the supported path),
 *   - a tools-mode seam (DSH_TOOLS_MODE; Code Mode per process),
 *   - agent-presets roster + code-runtime rows,
 *   - the phx-eval-driver row (src/eval/driver-plugin.mjs), whose apply()
 *     creates the agent, mounts the arm preset, runs the task, and writes
 *     the fail-closed run record (phoenix.eval.run.v1) to --out.
 *
 * This wrapper only covers composition + boot; everything after boot runs
 * inside the driver row's scoped context, exactly like dsh-headless.
 *
 * Exit codes: 0 completed · 1 loop error · 2 boot/setup failure · 3 killed
 */
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const SRC = dirname(fileURLToPath(import.meta.url))
const DRIVER = join(SRC, 'driver-plugin.mjs')

const argv = process.argv.slice(2)
function arg(name, fallback) {
  const i = argv.indexOf(`--${name}`)
  if (i === -1 || i + 1 >= argv.length) {
    if (fallback !== undefined) return fallback
    console.error(`run-one: missing --${name}`)
    process.exit(2)
  }
  return argv[i + 1]
}

const presetId = arg('preset')
const taskId = arg('task')
const runIdx = Number(arg('run', '0'))
const worktree = resolve(arg('worktree'))
const promptFile = resolve(arg('prompt-file'))
const dshHome = resolve(arg('dsh-home'))
const outFile = resolve(arg('out'))
const budgetMs = Number(arg('budget-ms', String(45 * 60000)))
const checkout = process.env.PHOENIX_DSH_CHECKOUT
  ? resolve(process.env.PHOENIX_DSH_CHECKOUT)
  : join(process.env.USERPROFILE ?? '', 'AppData', 'Local', 'npm-cache', '_npx', '1e7f6d9597241db0')

const startedAt = new Date().toISOString()

function writeRecord(partial) {
  try {
    mkdirSync(join(outFile, '..'), { recursive: true })
    writeFileSync(outFile, JSON.stringify({
      schema: 'phoenix.eval.run.v1',
      presetId, taskId, run: runIdx, startedAt,
      finishedAt: new Date().toISOString(),
      wallMs: Date.now() - new Date(startedAt).getTime(),
      checkout,
      sessionId: null,
      killedByBudget: false,
      ...partial,
    }, null, 2))
  } catch (err) {
    console.error(`run-one: cannot write record: ${err?.message ?? err}`)
    process.exit(2)
  }
}

function fail(exitCode, message) {
  console.error(`run-one: ${message}`)
  writeRecord({ ok: false, exitCode, error: String(message) })
  process.exit(exitCode)
}

try {
  process.env.DSH_HOME = dshHome
  if (!existsSync(promptFile)) fail(2, `prompt file missing: ${promptFile}`)
  const basePatch = join(checkout, 'node_modules', '@deepseek-ai', 'dsh-base', 'cordis.patch.yml')
  if (!existsSync(basePatch)) fail(2, `dsh-base patch missing at ${basePatch} — checkout pin broken`)
  if (!existsSync(DRIVER)) fail(2, `driver plugin missing at ${DRIVER}`)

  // Root document: empty entry list; the whole tree composes as patches.
  // The file MUST live inside the checkout's node_modules: the host baseUrl
  // (dir of the root config) is what preset mounts use to resolve bare
  // package names (dsh-agent-presets captures agentCtx.baseUrl), and a dir
  // inside node_modules resolves them exactly like the shipped profiles do.
  const composedPath = join(checkout, 'node_modules', '.phx-eval', `root-${process.pid}.cordis.yml`)
  mkdirSync(join(composedPath, '..'), { recursive: true })
  writeFileSync(composedPath, '[]\n')

  const M = (pkg) => pathToFileURL(join(checkout, 'node_modules', '@deepseek-ai', pkg, 'lib', 'index.js')).href
  const { boot, loadOptionalPatches } = await import(M('dsh-app-boot'))

  const baseRows = loadOptionalPatches('phx-eval', basePatch) ?? []
  if (baseRows.length === 0) fail(2, 'dsh-base patch parsed to zero rows — pin/loader mismatch')
  // boot() takes a FLAT patch list (the shape loadOptionalPatches returns and
  // dsh's allPatches() builds): id-targeted overrides plus insert rows, which
  // the include applies over the empty root document.
  const patches = [
    ...baseRows,
    { id: 'tools', config: { mode: process.env.DSH_TOOLS_MODE || 'native' } },
    {
      insert: [
        { id: 'agent-presets', name: '@deepseek-ai/dsh-agent-presets', config: { default: 'standard' } },
        { id: 'code-runtime', name: '@deepseek-ai/dsh-code-runtime-worker-thread' },
        {
          id: 'phx-eval-driver',
          name: DRIVER,
          inject: ['agents', 'agentDefaultModel', 'sessions', 'agentPresets', 'settings'],
          config: {
            presetId, taskId, run: runIdx, worktree, taskFile: promptFile, outFile, budgetMs,
          },
        },
      ],
    },
  ]

  const bareBase = pathToFileURL(join(checkout, 'node_modules') + '/').href
  let ctx = null
  try {
    ctx = await boot('phx-eval', composedPath, patches, undefined, bareBase)
  } catch (errUrl) {
    console.error(`run-one: boot with file-URL base failed (${errUrl?.message ?? errUrl}) — retrying with plain path`)
    ctx = await boot('phx-eval', composedPath, patches, undefined, join(checkout, 'node_modules'))
  }
  // The driver row's apply() runs to completion before the loader settles;
  // it owns the record and the process exit code.
  await ctx.get('loader')?.await()
  console.error('run-one: loader settled without driver record — unexpected; exiting 2')
  process.exit(2)
} catch (err) {
  fail(2, `unhandled boot failure: ${err?.message ?? err}`)
}
