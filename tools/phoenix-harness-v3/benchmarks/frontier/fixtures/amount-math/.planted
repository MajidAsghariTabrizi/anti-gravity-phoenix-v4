/**
 * Frontier-eval fixture: planted bug in integer amount math.
 * Phoenix money-path rule: integer math only — never floats.
 * The planted bug: fee computation uses a float division, so a large
 * amount loses precision (integer overflow of exactness).
 */

/**
 * Compute the flash premium fee in wei for an amount.
 * @param {string|number} amountWei - token amount in wei
 * @param {number} feeBps - fee in basis points (integer)
 * @returns {string} fee in wei, exact integer math
 */
export function flashPremium(amountWei, feeBps) {
  const amount = BigInt(amountWei)
  const fee = BigInt(feeBps)
  // PLANTED BUG: float path loses exactness for large amounts
  const feeWei = Number(amount) * (Number(fee) / 10000)
  return BigInt(Math.floor(feeWei)).toString()
}

/**
 * Exact integer reference implementation (control).
 */
export function flashPremiumExact(amountWei, feeBps) {
  const amount = BigInt(amountWei)
  const fee = BigInt(feeBps)
  return ((amount * fee) / 10000n).toString()
}
