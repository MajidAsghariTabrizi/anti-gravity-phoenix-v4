/**
 * Phoenix Harness V3 eval — the per-task DRIVER PLUGIN row.
 *
 * Mirrors the shipped dsh-headless architecture: a composition row whose
 * apply() receives a row-scoped context where the core registries resolve.
 * It creates ONE agent with the arm preset mounted, drives one task prompt
 * to quiescence, flushes the session, and writes the fail-closed run record
 * (phoenix.eval.run.v1) to config.outFile. Never prints the final assistant
 * text to stdout; stdout carries status lines only.
 *
 * All @deepseek-ai imports resolve through process.env.PHOENIX_DSH_CHECKOUT
 * (this file lives in the phoenix repo, which has no node_modules), so the
 * loader's bare-module base cannot serve them.
 *
 * Exit codes: 0 completed · 1 loop error · 2 boot/setup failure · 3 killed
 */
import { pathToFileURL } from 'node:url'
import { join } from 'node:path'
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { randomUUID } from 'node:crypto'

const checkout = process.env.PHOENIX_DSH_CHECKOUT
const M = (pkg) => pathToFileURL(join(checkout, 'node_modules', '@deepseek-ai', pkg, 'lib', 'index.js')).href

function writeRecord(config, startedAt, partial) {
  try {
    mkdirSync(join(config.outFile, '..'), { recursive: true })
    writeFileSync(config.outFile, JSON.stringify({
      schema: 'phoenix.eval.run.v1',
      presetId: config.presetId, taskId: config.taskId, run: Number(config.run ?? 0), startedAt,
      finishedAt: new Date().toISOString(),
      wallMs: Date.now() - new Date(startedAt).getTime(),
      checkout,
      sessionId: partial.sessionId ?? null,
      killedByBudget: partial.killedByBudget === true,
      ...partial,
    }, null, 2))
  } catch (err) {
    console.error(`phx-eval-driver: cannot write record: ${err?.message ?? err}`)
    process.exit(2)
  }
}

export const name = 'phx-eval-driver'
export const inject = ['agents', 'agentDefaultModel', 'sessions', 'agentPresets', 'settings']

export async function apply(ctx, config) {
  console.error('[phx-eval-driver] apply entered')
  const startedAt = new Date().toISOString()
  let killedByBudget = false
  const budgetMs = Number(config.budgetMs ?? 45 * 60000)

  // Belt-and-braces self-budget: if the parent watchdog dies, the child still
  // cannot run away.
  const budgetTimer = setTimeout(() => {
    killedByBudget = true
    console.error(`phx-eval-driver: task budget ${budgetMs}ms exceeded — killing self`)
    writeRecord(config, startedAt, { ok: false, exitCode: 3, error: 'task budget exceeded (self-kill)', killedByBudget: true })
    process.exit(3)
  }, budgetMs + 5 * 60000)
  budgetTimer.unref?.()

  const fail = (exitCode, message, partial = {}) => {
    console.error(`phx-eval-driver: ${message}`)
    writeRecord(config, startedAt, { ok: false, exitCode, error: String(message), ...partial })
    process.exit(exitCode)
  }

  try {
    const { installModelSelection } = await import(M('dsh-agent'))
    const { createUserMessage } = await import(M('dsh-llm'))
    const { SessionId } = await import(M('dsh-session'))

    const taskFile = config.taskFile
    if (!existsSync(taskFile)) fail(2, `task prompt file missing: ${taskFile}`)
    const taskText = readFileSync(taskFile, 'utf8')

    const agents = ctx.get('agents')
    const sessions = ctx.get('sessions')
    const defaultModel = ctx.get('agentDefaultModel')
    const agentPresets = ctx.get('agentPresets')
    if (!agents || !sessions || !defaultModel) fail(2, 'composition missing agents/sessions/agentDefaultModel')
    if (!agentPresets) fail(2, 'composition missing the agent-presets roster row')

    // The settings user layer settles ASYNCHRONOUSLY after boot: the service
    // reads the composition base (flash) first and only re-points at the
    // settings.yaml section a moment later. Never create the eval agent on
    // the base default — wait until the selection matches the settings
    // section (or a hard cap), then fail closed.
    const settingsSvc = ctx.get('settings')
    const want = (() => {
      try { return settingsSvc?.section('agent-default-model') ?? null } catch { return null }
    })()
    const matches = (sel) => !want || (
      sel.provider === want.provider &&
      sel.model === want.model &&
      (sel.reasoningEffort ?? null) === (want.reasoningEffort ?? null))
    let selection = defaultModel.currentSelection()
    if (!matches(selection)) {
      for (let i = 0; i < 40; i++) {
        await new Promise((r) => setTimeout(r, 500))
        selection = defaultModel.currentSelection()
        if (matches(selection)) break
      }
      if (!matches(selection)) {
        fail(2, `model selection never reached the settings layer (want=${JSON.stringify(want)} got=${JSON.stringify(selection)})`)
      }
    }
    console.error(`phx-eval-driver: preset=${config.presetId} task=${config.taskId} run=${config.run} model=${selection.provider}/${selection.model} effort=${selection.reasoningEffort ?? '(default)'}`)

    // Fail closed BEFORE creating the agent if the arm preset cannot mount.
    // preset '' / 'none' = bare judge agent (no Phoenix preset).
    const presetId = config.presetId ?? ''
    const hasPreset = presetId !== '' && presetId !== 'none'
    if (hasPreset) {
      try {
        await agentPresets.standingKeyFor(presetId)
      } catch (errMount) {
        fail(2, `preset "${presetId}" mount validation failed: ${errMount?.message ?? errMount}`)
      }
    }

    const sessionId = SessionId(`eval-${presetId}-${config.taskId}-r${config.run}-${randomUUID().slice(0, 8)}`)
    const { agent } = await agents.create({
      sessionId,
      meta: { cwd: config.worktree, agentPreset: hasPreset ? presetId : undefined },
      agentOptions: { provider: selection.provider, model: selection.model },
      setup: async (agentCtx) => {
        if (hasPreset) await agentPresets.mount(agentCtx, presetId)
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
    console.error(`phx-eval-driver: done kind=${reason?.kind ?? '?'} textLen=${text.length} seq=${agent.session.seq}`)
    writeRecord(config, startedAt, {
      ok: completed,
      exitCode: completed ? 0 : 1,
      sessionId,
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
}
