/**
 * Phoenix Harness V3 eval — anonymized A/B judge (Phase 2/4).
 *
 * The judge is a BARE harness agent (no Phoenix preset — only base tools):
 * it sees two anonymized outputs labelled OUTPUT X / OUTPUT Y with the task
 * rubric, and must return a strict JSON verdict. The arm order is shuffled
 * per task by the caller; the judge never sees preset ids, session ids, or
 * run metadata. Verdicts are parsed fail-closed: unparseable JSON is not a
 * verdict.
 */
export function judgePrompt(taskDef, aText, bText) {
  const rubric = [
    ...(taskDef.rubric.correctness ?? []).map((r) => `- CORRECTNESS: ${r}`),
    ...(taskDef.rubric.safety ?? []).map((r) => `- SAFETY: ${r}`),
    ...(taskDef.rubric.evidence ?? []).map((r) => `- EVIDENCE: ${r}`),
  ].join('\n')
  return [
    'You are an impartial evaluator in a double-blind harness comparison.',
    'Two agent outputs follow, labelled OUTPUT X and OUTPUT Y (randomly assigned).',
    'Judge ONLY against the task and rubric below. Ignore output length, formatting taste, and the order presented.',
    '',
    `TASK: ${taskDef.name}\n${taskDef.prompt}`,
    '',
    `RUBRIC:\n${rubric}`,
    '',
    'OUTPUT X:',
    aText.slice(0, 30000),
    '',
    'OUTPUT Y:',
    bText.slice(0, 30000),
    '',
    'Reply with EXACTLY one JSON object, no other text:',
    '{"verdict":"x_wins"|"y_wins"|"tie"|"both_fail","winner":"x"|"y"|null,"qualityX":1-5,"qualityY":1-5,"correctnessX":true|false,"correctnessY":true|false,"notes":"<=200 chars"}',
    'qualityX/Y: 1=wrong or harmful, 3=partial, 5=complete and well-evidenced.',
    'correctnessX/Y: whether the output satisfies the task\'s correctness rubric.',
    'verdict=both_fail when neither output satisfies the correctness rubric; verdict=tie when both fully satisfy it and neither is materially better.',
  ].join('\n')
}

export function parseVerdict(text) {
  const candidate = String(text ?? '')
  // The bare agent may wrap the JSON in a code fence.
  const fenced = candidate.match(/```(?:json)?\s*([\s\S]*?)```/i)
  const body = (fenced ? fenced[1] : candidate).trim()
  const first = body.indexOf('{')
  const last = body.lastIndexOf('}')
  if (first === -1 || last === -1 || last <= first) return null
  try {
    const v = JSON.parse(body.slice(first, last + 1))
    const verdicts = new Set(['x_wins', 'y_wins', 'tie', 'both_fail'])
    if (!verdicts.has(v.verdict)) return null
    if (![1, 2, 3, 4, 5].includes(v.qualityX) || ![1, 2, 3, 4, 5].includes(v.qualityY)) return null
    if (typeof v.correctnessX !== 'boolean' || typeof v.correctnessY !== 'boolean') return null
    return {
      verdict: v.verdict,
      winner: v.verdict === 'x_wins' ? 'x' : v.verdict === 'y_wins' ? 'y' : null,
      qualityX: v.qualityX, qualityY: v.qualityY,
      correctnessX: v.correctnessX, correctnessY: v.correctnessY,
      notes: String(v.notes ?? '').slice(0, 200),
    }
  } catch {
    return null
  }
}

/**
 * Map a judge verdict back to arm names given the shuffle order.
 * order = {x: arm, y: arm}. Returns {armResult: {control, candidate}, raw}.
 */
export function mapVerdict(raw, order) {
  const controlWin = raw.verdict === 'x_wins' && order.x === 'control' || raw.verdict === 'y_wins' && order.y === 'control'
  const candidateWin = raw.verdict === 'x_wins' && order.x === 'candidate' || raw.verdict === 'y_wins' && order.y === 'candidate'
  const correctness = {
    control: order.x === 'control' ? raw.correctnessX : raw.correctnessY,
    candidate: order.x === 'candidate' ? raw.correctnessX : raw.correctnessY,
  }
  const quality = {
    control: order.x === 'control' ? raw.qualityX : raw.qualityY,
    candidate: order.x === 'candidate' ? raw.qualityX : raw.qualityY,
  }
  return { controlWin, candidateWin, tie: raw.verdict === 'tie', bothFail: raw.verdict === 'both_fail', correctness, quality, notes: raw.notes }
}
