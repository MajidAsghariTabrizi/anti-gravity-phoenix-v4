/**
 * L-003/L-006 native-tool safety tests: remote allowlist refusals, SQL
 * statement gate, dispatch authorization chain. These paths REFUSE before
 * any network access, so the tests are hermetic.
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { pathToFileURL } from 'node:url'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const REMOTE = pathToFileURL(join(ROOT, 'src', 'tools-native', 'remote.js')).href
const CI = pathToFileURL(join(ROOT, 'src', 'tools-native', 'ci.js')).href

test('phoenix_remote refuses sudo, pipes, redirects, unknown commands', async () => {
  const { remoteTool } = await import(REMOTE)
  const tool = remoteTool()
  const bad = [
    'sudo docker ps',
    'docker ps | grep phoenix',
    'rm -rf /opt/phoenix',
    'cat /etc/shadow',
    'docker exec -it postgres bash',
    'systemctl restart phoenix-autonomous',
    'curl http://169.254.169.254/latest/meta-data',
  ]
  for (const cmd of bad) {
    const out = await tool.execute({ command: cmd })
    assert.ok(/REFUSED/.test(out), `"${cmd}" must be refused, got: ${out.slice(0, 80)}`)
  }
  // allowlisted commands pass the gate (no assertion about ssh result — hermetic)
  const allowed = await tool.execute({ command: 'docker ps' })
  assert.ok(!/REFUSED/.test(allowed), 'docker ps must pass the allowlist gate')
})

test('phoenix_sql_readonly blocks DML/DDL/chaining/multi-statement', async () => {
  const { sqlReadonlyTool } = await import(`${REMOTE}?t=${Date.now()}`)
  const tool = sqlReadonlyTool()
  for (const q of [
    'DROP TABLE execution_requests',
    'UPDATE execution_requests SET status=1',
    'SELECT 1; DROP TABLE x',
    'DELETE FROM candidates',
    'COPY candidates TO stdout',
    'SELECT * FROM candidates; SELECT * FROM attempts',
  ]) {
    const out = await tool.execute({ query: q })
    assert.ok(/REFUSED/.test(out), `"${q}" must be refused, got: ${out.slice(0, 80)}`)
  }
  // a legitimate SELECT passes the gate (may fail at ssh — hermetic: only check it was not refused)
  const ok = await tool.execute({ query: 'SELECT count(*) FROM execution_requests' })
  assert.ok(!/REFUSED/.test(ok), 'single SELECT must pass the gate')
})

test('phoenix_sql_readonly composes attached separator and shell-quoted query (L-012 regression)', async () => {
  const { buildSqlRemoteCommand, sqlReadonlyTool } = await import(`${REMOTE}?t=${Date.now()}`)
  // psql -F separator must be ATTACHED (-F'|'), never a separate argv element:
  // ssh joins argv[1..] with spaces and the remote shell would treat a bare '|'
  // as a pipe, leaving psql without a separator argument.
  const cmd = buildSqlRemoteCommand('app-postgres-1', 'phoenix_app', 'phoenix_v5_abc123', "SELECT 'a' AS v FROM t WHERE x > 1")
  assert.ok(cmd.startsWith('docker exec -i -u postgres app-postgres-1 psql -U '), `must exec as OS user postgres with explicit -U role, got: ${cmd}`)
  assert.ok(cmd.includes("-F'|'"), `separator must be attached to -F, got: ${cmd}`)
  assert.ok(!cmd.includes("-F '|'"), 'no detached separator token (-F with bare pipe arg)')
  // the role, database and query are single-quoted with embedded quotes shell-escaped
  assert.ok(cmd.includes("-U 'phoenix_app'"), `role must be quoted, got: ${cmd}`)
  assert.ok(cmd.includes("-d 'phoenix_v5_abc123'"), `db must be quoted, got: ${cmd}`)
  assert.ok(cmd.includes("-c 'SELECT '\\''a'\\'' AS v FROM t WHERE x > 1'"), `query must be quoted/escaped, got: ${cmd}`)
  // no unquoted shell metacharacters anywhere in the command
  const unquoted = cmd.replace(/'[^']*'/g, '').replace(/'\\''/g, '')
  assert.ok(!/[|;&<>`$]/.test(unquoted), `no unquoted shell metacharacters, got: ${cmd}`)
  // transport-unsafe characters are refused before any ssh
  const tool = sqlReadonlyTool()
  for (const q of ["SELECT 'a|b'", 'SELECT 1\nSELECT 2', 'SELECT `x` FROM t']) {
    const out = await tool.execute({ query: q })
    assert.ok(/REFUSED/.test(out), `"${q}" must be refused, got: ${out.slice(0, 80)}`)
  }
})

test('phoenix_release_dispatch refuses without mission/approval/ack and requires dry-run', async () => {
  const tmp = mkdtempSync(join(tmpdir(), 'phx-v3-dispatch-'))
  try {
    const { releaseDispatchTool } = await import(`${CI}?t=${Date.now()}`)
    const missionModule = await import(pathToFileURL(join(ROOT, 'src', 'mission.js')).href)
    const tool = releaseDispatchTool(tmp, missionModule)
    const exec = { agent: { session: { id: 's1' } } }
    // 1. no mission
    let out = await tool.execute({ command: 'release_provenance.py', args: ['--dry-run'], ack: 'x' }, exec)
    assert.ok(/REFUSED.*no mission/.test(out))
    // 2. mission but not prod_mutation
    missionModule.writeMission(tmp, 's1', missionModule.compileMission({ objective: 'x' }).spec)
    out = await tool.execute({ command: 'release_provenance.py', args: ['--dry-run'], ack: 'x' }, exec)
    assert.ok(/REFUSED.*riskTier/.test(out))
    // 3. prod_mutation but no owner approval
    missionModule.writeMission(tmp, 's1', missionModule.compileMission({ objective: 'x', riskTier: 'prod_mutation' }).spec)
    out = await tool.execute({ command: 'release_provenance.py', args: ['--dry-run'], ack: 'x' }, exec)
    assert.ok(/REFUSED.*owner approval/.test(out))
    // 4. approval but wrong ack
    const spec = missionModule.readMission(tmp, 's1').spec
    spec.ownerApproval = { approvedAt: new Date().toISOString(), by: 'owner', scope: 'test' }
    missionModule.writeMission(tmp, 's1', spec)
    out = await tool.execute({ command: 'release_provenance.py', args: ['--dry-run'], ack: 'wrong' }, exec)
    assert.ok(/REFUSED.*ack/.test(out))
    // 5. everything right but no dry-run and no plan_ack -> refuses to execute
    const { mkdirSync, writeFileSync } = await import('node:fs')
    mkdirSync(join(tmp, 'scripts'), { recursive: true })
    writeFileSync(join(tmp, 'scripts', 'release_provenance.py'), 'print("canonical stub")\n')
    out = await tool.execute({ command: 'release_provenance.py', args: [], ack: 'x' }, exec)
    assert.ok(/REFUSED to execute/.test(out))
    // 6. unknown command refused even with full authorization
    out = await tool.execute({ command: 'rm-rf.sh', args: ['--dry-run'], ack: 'x' }, exec)
    assert.ok(/REFUSED.*unknown command/.test(out))
  } finally {
    rmSync(tmp, { recursive: true, force: true })
  }
})

test('native tools use argument arrays (no shell interpolation) — structural', async () => {
  // The module files must never contain template-built shell strings for ssh.
  const { readFileSync } = await import('node:fs')
  const remoteSrc = readFileSync(join(ROOT, 'src', 'tools-native', 'remote.js'), 'utf8')
  assert.ok(!/`ssh\s+\$\{/.test(remoteSrc), 'no shell-string ssh construction')
  assert.ok(!/exec\(/.test(remoteSrc), 'no bare exec() calls')
})
