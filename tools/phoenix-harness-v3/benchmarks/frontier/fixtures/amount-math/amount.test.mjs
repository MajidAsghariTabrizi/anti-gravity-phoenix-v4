/**
 * Fixture test for the planted-bug fix task. This test FAILS against the
 * buggy implementation and PASSES after the fix (exact integer math).
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import { flashPremium, flashPremiumExact } from './buggy_amount.mjs'

test('flash premium is exact integer math (no float precision loss)', () => {
  const cases = [
    ['1000000000000000000', 9], // 1 ETH, 9 bps
    ['115792089237316195423570985008687907853269984665640564039457584007913129639935', 30], // near uint256 max
    ['263211006474615', 5],
    ['999999999999999999999999', 1],
  ]
  for (const [amount, bps] of cases) {
    assert.equal(flashPremium(amount, bps), flashPremiumExact(amount, bps), `precision loss at amount=${amount} bps=${bps}`)
  }
})

test('flash premium is zero for zero bps and zero amount', () => {
  assert.equal(flashPremium('0', 30), '0')
  assert.equal(flashPremium('123456789', 0), '0')
})

test('flash premium uses integer math only (never Number floats in the hot path)', () => {
  const src = flashPremium.toString()
  assert.ok(!/Number\(/.test(src), 'float conversion found in implementation')
})
