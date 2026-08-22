#!/usr/bin/env node
/**
 * Phoenix Harness V3 eval — one-shot per-task child driver.
 *
 * Boots the pinned DSH checkout (base host composition + agent-presets row),
 * creates ONE agent with the arm's preset mounted under the worktree cwd,
 * drives one task prompt to quiescence, flushes the session, and writes a
 * fail-closed run record (phoenix.eval.run.v1) to --out. Never prints the
 * final assistant text to stdout (evidence stays in the run record); stdout
 * carries status lines only.
 *
 * Mirrors dsh-headless/lib/index.js run() for the loop lifecycle, plus the
 * preset mount from the agent-presets registry (the base host composition
 * does not ship the roster row).
 *
 * Exit codes: 0 completed · 1 loop error · 2 boot/setup failure · 3 killed
 */
import { pathToFileURL } from 'node:url'
import { join, resolve } from 'node:path'
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { randomUUID } from 'node:crypto'

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
function flag(name) {
  return argv.includes(`--${name}`)
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
let sessionId = null
let killedByBudget = false

function writeRecord(partial) {
  try {
    mkdirSync(join(outFile, '..'), { recursive: true })
    writeFileSync(outFile, JSON.stringify({
      schema: 'phoenix.eval.run.v1',
      presetId, taskId, run: runIdx, startedAt,
      finishedAt: new Date().toISOString(),
      wallMs: Date.now() - new Date(startedAt).getTime(),
      checkout,
      sessionId,
      killedByBudget,
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

// Belt-and-braces self-budget: if the parent watchdog dies, the child still
// cannot run away.
const budgetTimer = setTimeout(() => {
  killedByBudget = true
  console.error(`run-one: task budget ${budgetMs}ms exceeded — killing self`)
  writeRecord({ ok: false, exitCode: 3, error: 'task budget exceeded (self-kill)' })
  process.exit(3)
}, budgetMs + 5 * 60000)
budgetTimer.unref?.()

try {
  process.env.DSH_HOME = dshHome
  if (!existsSync(promptFile)) fail(2, `prompt file missing: ${promptFile}`)
  const taskText = readFileSync(promptFile, 'utf8')
  const basePatch = join(checkout, 'node_modules', '@deepseek-ai', 'dsh-base', 'cordis.patch.yml')
  if (!existsSync(basePatch)) fail(2, `dsh-base patch missing at ${basePatch} — checkout pin broken`)

  // Compose the child's host config: the untouched dsh-base patch + the
  // agent-presets roster row + the code-runtime row (Code Mode transport).
  const composed = `${readFileSync(basePatch, 'utf8')}
- insert:
    - id: agent-presets
      name: '@deepseek-ai/dsh-agent-presets'
    - id: code-runtime
      name: '@deepseek-ai/dsh-code-runtime-worker-thread'
`
  const composedPath = join(dshHome, `phx-eval-composed-${process.pid}.cordis.yml`)
  mkdirSync(dshHome, { recursive: true })
  writeFileSync(composedPath, composed)

  const M = (pkg) => pathToFileURL(join(checkout, 'node_modules', '@deepseek-ai', pkg, 'lib', 'index.js')).href
  const { boot } = await import(M('dsh-app-boot'))
  const { installModelSelection } = await import(M('dsh-agent'))
  const { createUserMessage } = await import(M('dsh-llm'))
  const { SessionId } = await import(M('dsh-session'))

  const bareBase = pathToFileURL(join(checkout, 'node_modules') + '/').href
  let ctx = null
  try {
    ctx = await boot('phx-eval', composedPath, [], undefined, bareBase)
  } catch (errUrl) {
    console.error(`run-one: boot with file-URL base failed (${errUrl?.message ?? errUrl}) — retrying with plain path`)
    ctx = await boot('phx-eval', composedPath, [], undefined, join(checkout, 'node_modules'))
  }
  await ctx.get('loader')?.await()

  const agents = ctx.get('agents')
  const sessions = ctx.get('sessions')
  const defaultModel = ctx.get('agentDefaultModel')
  if (!agents || !sessions || !defaultModel) fail(2, 'host composition missing agents/sessions/agentDefaultModel')
  if (!ctx.agentPresets) fail(2, 'host composition missing the agent-presets roster row')

  const selection = defaultModel.currentSelection()
  console.error(`run-one: preset=${presetId} task=${taskId} run=${runIdx} model=${selection.provider}/${selection.model} effort=${selection.reasoningEffort ?? '(default)'}`)

  // Fail closed BEFORE creating the agent if the arm preset cannot mount.
  // preset 'none' = bare judge agent (no Phoenix preset, base tools only).
  const hasPreset = presetId !== '' && presetId !== 'none'
  if (hasPreset) {
    try {
      await ctx.agentPresets.standingKeyFor(presetId)
    } catch (errMount) {
      fail(2, `preset "${presetId}" mount validation failed: ${errMount?.message ?? errMount}`)
    }
  }

  sessionId = SessionId(`eval-${presetId}-${taskId}-r${runIdx}-${randomUUID().slice(0, 8)}`)
  const { agent } = await agents.create({
    sessionId,
    meta: { cwd: worktree, agentPreset: hasPreset ? presetId : undefined },
    agentOptions: { provider: selection.provider, model: selection.model },
    setup: async (agentCtx) => {
      if (hasPreset) await ctx.agentPresets.mount(agentCtx, presetId)
      installModelSelection(agentCtx, { current: selection, assembled: undefined })
    },
  })

  await agent.whenIdle()
  const firstSeq = agent.session.seq
  agent.followup(createUserMessage({
    content: [{ type: 'text', text: taskText }],
    source: { kind: 'user' },
  }))
  await agent.whenIdle()
  await sessions.flush(agent.session)

  // Fold the outcome exactly like dsh-headless, plus a compact event digest.
  let text = ''
  let reason
  let started = false
  const digest = { types: {}, tools: {} }
  for (const event of agent.session.events) {
    if (event.seq < firstSeq) continue
    if (event.type === 'turn/start') { started = true; continue }
    if (!started) continue
    digest.types[event.type] = (digest.types[event.type] ?? 0) + 1
    if (event.type === 'assistant/message') {
      const joined = event.data.message.content.filter((b) => b.type === 'text').map((b) => b.text).join('')
      if (joined !== '') text = joined
    }
    if (event.type === 'turn/end') reason = event.data.reason
    if (event.type === 'tool/start' || event.type === 'tool/end') {
      const t = event.data?.tool ?? event.data?.name ?? '(unknown)'
      digest.tools[t] = (digest.tools[t] ?? 0) + 1
    }
  }
  const completed = reason?.kind === 'completed'
  console.error(`run-one: done kind=${reason?.kind ?? '?'} textLen=${text.length} seq=${agent.session.seq}`)
  writeRecord({
    ok: completed,
    exitCode: completed ? 0 : 1,
    finalText: text.slice(0, 200000),
    finalTextLen: text.length,
    reason,
    digest,
    model: { provider: selection.provider, model: selection.model, reasoningEffort: selection.reasoningEffort ?? null },
  })
  process.exit(completed ? 0 : 1)
} catch (err) {
  fail(2, `unhandled driver failure: ${err?.message ?? err}`)
}
