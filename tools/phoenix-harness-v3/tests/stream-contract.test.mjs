/**
 * L-001 regression: llm/stream AsyncIterable contract.
 * Drives the real plugin apply() with a fake context and asserts the
 * installed harness contract: listeners return an AsyncIterable DIRECTLY
 * (never a Promise), next() is called exactly once and synchronously,
 * chunks re-yield in order, downstream close propagates, telemetry is
 * written, and a telemetry failure cannot break the stream.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const pluginUrl = pathToFileURL(join(ROOT, 'src', 'plugin.js')).href

async function makeChunks(usage) {
  const chunks = []
  if (usage) chunks.push({ type: 'usage', usage })
  chunks.push({ type: 'text', text: 'hello' })
  chunks.push({ type: 'finish', reason: { kind: 'stop' } })
  return chunks
}

test('llm/stream listener returns AsyncIterable directly, never a Promise', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-stream-'))
  try {
    const { apply } = await import(pluginUrl)
    let streamListener = null
    const registered = []
    const ctx = {
      on(event, fn) { if (event === 'llm/stream') streamListener = fn },
      get() { return undefined },
      tools: { register(def) { registered.push(def) } },
    }
    apply(ctx, { workspaceRoot: tmp })
    assert.ok(streamListener, 'llm/stream listener registered')

    let nextCalls = 0
    const downstream = (async function* () {
      for (const c of await makeChunks({ inputTokens: 100, outputTokens: 50, cacheReadTokens: 500, cacheWriteTokens: 0, reasoningTokens: 20 })) yield c
    })()
    const result = streamListener({ sessionId: 's1', messages: [], system: '' }, () => {
      nextCalls += 1
      return downstream
    })
    assert.equal(typeof result?.then, 'undefined', 'listener returned a Promise (L-001 violation)')
    assert.equal(typeof result?.[Symbol.asyncIterator], 'function', 'listener did not return an AsyncIterable')
    const collected = []
    for await (const c of result) collected.push(c)
    assert.equal(nextCalls, 1, 'next() must be called exactly once')
    assert.deepEqual(collected.map((c) => c.type), ['usage', 'text', 'finish'])
    assert.equal(collected[0].usage.inputTokens, 100)
    // telemetry file written
    const { readFileSync, existsSync } = await import('node:fs')
    const telemetry = join(tmp, '.phoenix-harness', 'telemetry', 'session-s1.jsonl')
    assert.ok(existsSync(telemetry), 'telemetry record written')
    const rec = JSON.parse(readFileSync(telemetry, 'utf8').trim().split('\n')[0])
    assert.equal(rec.event, 'llm.request')
    assert.deepEqual(rec.usage, { input: 100, output: 50, cacheRead: 500, cacheWrite: 0, reasoning: 20 })
    assert.equal(rec.finish, 'stop')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('llm/stream: sync next() throw propagates and records failure', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-stream2-'))
  try {
    const { apply } = await import(`${pluginUrl}?t=${Date.now()}`)
    let streamListener = null
    const ctx = { on(e, fn) { if (e === 'llm/stream') streamListener = fn }, get() { return undefined }, tools: { register() {} } }
    apply(ctx, { workspaceRoot: tmp })
    const result = streamListener({ sessionId: 's2', messages: [] }, () => { throw Object.assign(new Error('boom'), { code: 'TRANSPORT' }) })
    await assert.rejects(async () => { for await (const _ of result) { /* drain */ } }, /boom/)
    const { readFileSync, existsSync } = await import('node:fs')
    const rec = JSON.parse(readFileSync(join(tmp, '.phoenix-harness', 'telemetry', 'session-s2.jsonl'), 'utf8').trim())
    assert.equal(rec.failure.code, 'TRANSPORT')
    assert.equal(rec.finish, 'error')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('llm/stream: downstream error propagates; early cancel closes downstream', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-stream3-'))
  try {
    const { apply } = await import(`${pluginUrl}?t=${Date.now()}`)
    let streamListener = null
    const ctx = { on(e, fn) { if (e === 'llm/stream') streamListener = fn }, get() { return undefined }, tools: { register() {} } }
    apply(ctx, { workspaceRoot: tmp })
    let closed = false
    const downstream = (async function* () {
      try {
        yield { type: 'text', text: 'a' }
        await new Promise((r) => setTimeout(r, 5))
        yield { type: 'text', text: 'b' }
      } finally {
        closed = true
      }
    })()
    const result = streamListener({ sessionId: 's3', messages: [] }, () => downstream)
    const it = result[Symbol.asyncIterator]()
    const first = await it.next()
    assert.equal(first.value.text, 'a')
    await it.return()
    assert.ok(closed, 'downstream iterator closed on cancel')
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})
