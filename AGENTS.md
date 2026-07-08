# Anti-Gravity Phoenix v4 Agent Guardrails

This repository is a new project. The parent Anti-Gravity repository is read-only reference.

Rules for every future coding agent:

- All source, configuration, fixtures, tests, docs, scripts, and generated artifacts belong under `anti-gravity-phoenix-v4/`.
- Do not modify, move, delete, reformat, or migrate files from the older parent project.
- The hot decision path must not make external public RPC reads. All read RPC usage goes through `rpc-gateway`.
- Do not guess protocol addresses, ABIs, feed formats, callback semantics, sequencer methods, or gas behavior.
- Record protocol assumptions and source versions in `docs/DEPENDENCIES.md`.
- Never claim a live integration or fork test passed unless it actually ran.
- Shadow mode is the default. `LIVE_EXECUTION=false` is the default.
- No Python belongs in the latency-critical search path.
- Use integer math for token amounts, sqrt prices, ticks, liquidity, gas costs, flash premiums, and profit.
- Do not use floats for on-chain quantities or opportunity accounting.
- Protocol math requires tests.
- The executor must preserve exact fee tier, pool, direction, asset, amount, and min-profit details from detection through execution.
- Transaction submission is not realized profit. Realized PnL comes only from reconciliation data.
- Never log private keys, raw secret environment variables, bearer tokens, webhook URLs, or RPC credentials.
- `.env` stays ignored. `.env.example` contains placeholders only.
- Run available formatting, linting, unit tests, integration tests, and contract tests before completion.
- If a tool or credential is unavailable, build the component, provide mocks/fixtures, add auto-enabled integration tests, and document the exact command.
- Do not add liquidation, sandwich, frontrun, triangular, CEX, ML, Timeboost, Curve, Camelot, or broad blind-scanning strategies in v4.0.

