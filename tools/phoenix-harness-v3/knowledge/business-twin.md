# Business Twin Facts

Repo root: `anti-gravity-phoenix-v4`. Figures are quoted exactly as stated; each number
carries its source as-of date. Realized PnL is only the reconciled figure (all sources
report $0). Expected/modeled/shadow PnL is labeled and never "realized".

## 1. Why revenue is zero
FACT — Realized Net PnL = $0 (entries []). As-of 2026-08-02. `docs/evidence/platform-transition-20260802/ECONOMIC_LEDGER.json:9-10`.
FACT — Aave lane polls a 186k-borrower snapshot once a day (full sweep ≈31h) while liquidations are won in seconds after oracle updates. As-of 2026-08-20. `.agent-private/alpha-source-investigation/EXECUTIVE_REVENUE_VERDICT.md:4-5`.
FACT — Aave lane never ingests oracle events or liquidation logs; reviewed universe is WETH-debt-only. `EXECUTIVE_REVENUE_VERDICT.md:5-7`.
FACT — SVR/Atlas auction stream (where Aave's liquidation value flows) is subscribed real-time but NEVER BID IN: bidding path gated behind a direct-fork simulation that never passes. `EXECUTIVE_REVENUE_VERDICT.md:7-13`.
FACT — 0 of 4,556 direct forks pass economics, so the Atlas path never exercises. `.agent-private/alpha-source-investigation/SVR_ATLAS_REALITY.md:18-21`.
FACT — Liquidatable-window recall ≈2% (1 poll/day vs minutes-long windows); exact-evaluation recall 0/2 on verified winners; oracle-event recall 0% by construction. `.agent-private/alpha-source-investigation/FINAL_VERDICT.md:12-14`.
FACT — Production financial authority CLOSED at every layer: lanes disarmed/killed, executor paused, 0 attempts, 0 unresolved submissions, locks free. As-of 2026-08-20 ~16:09Z. `.agent-private/revenue-capture-program/00_BASELINE.md:35-43,62-64`.
FACT — Verdict: "The pipeline itself is excellent: fork-verified, fail-closed, honest. Its economics are zero because its triggers are hours late, its universe excludes every verified external win, and its participation in the live auction market is zero." `EXECUTIVE_REVENUE_VERDICT.md:10-13`.

## 2. Market no-alpha vs capability gap
FACT — CURRENT_MARKET_NO_ALPHA holds ONLY for the current supported universe at current costs: PROVEN (0/4,556 forks, gross ceilings below cost). As-of 2026-08-20. `FINAL_VERDICT.md:28-29`.
FACT — ADDRESSABLE_ALPHA_EXISTS: verified external wins ($0.27 + $10-12 class), the excluded direct universe (≈0.73 ETH/wk gross ceiling), and the live SVR/Atlas flow Phoenix already receives and never bids in. `FINAL_VERDICT.md:30-33`.
FACT — Distinction: no-alpha = current narrow WETH-debt / WETH/USDC direct lane genuinely below economics (0/4,556 forks). Capability gap = the two verified wins sit in the policy-excluded non-WETH universe and the SVR/Atlas lane is structurally blocked, not no-alpha. `FINAL_VERDICT.md:28-33`; `.agent-private/alpha-source-investigation/ADDRESSABLE_MARKET_PNL.md:5-11`.
FACT — "CURRENTLY_ADDRESSABLE ≈ $0/week (0 fork passes / 7d; all reviewed candidates below economics, PROVEN)." `ADDRESSABLE_MARKET_PNL.md:7`.
UNKNOWN — SVR/Atlas Arbitrum share of the ~$4M/wk cross-chain SVR recapture; lane size UNKNOWN until the 7-day shadow validation. `ADDRESSABLE_MARKET_PNL.md:10`; `EXECUTIVE_REVENUE_VERDICT.md:31`.

## 3. Addressable opportunity value
FACT — SMALL_FIX (event trigger + non-WETH pairs): ~$5-20/week gross class; net after gas ≈ $2-10/week. Basis: 2 confirmed wins ($0.27 + $10-12) over ~7d in a 6-borrower sample (ESTIMATED). `ADDRESSABLE_MARKET_PNL.md:8`.
FACT — ROUTE_EXPANSION: 0.73 ETH/wk gross ceiling (~$1.65k) → realistic net $100-400/wk, one non-recurring whale dominant (ESTIMATED). `ADDRESSABLE_MARKET_PNL.md:9`; `.agent-private/alpha-source-investigation/MISSING_ALPHA_LEDGER.md:17`.
FACT — NEW_STRATEGY (SVR/Atlas solver lane): UNKNOWN_DUE_TO_DATA_LIMIT; the market's active value channel (147 auctions/h, ~57% Aave-relevant; SVR ≈ $4M/wk cross-chain). `ADDRESSABLE_MARKET_PNL.md:10`; `SVR_ATLAS_REALITY.md:30`.
FACT — UNADDRESSABLE: ordering whitelists, private solver relationships, cross-chain latency races (UNKNOWN). `ADDRESSABLE_MARKET_PNL.md:11`.
FACT — Ranked: 1) SVR/Atlas auction lane — only channel with constant, current, real liquidation-value flow on Aave Arbitrum; 2) non-WETH direct liquidations (small but real); 3) everything else Phoenix does is unprofitable-by-economics. `ADDRESSABLE_MARKET_PNL.md:13-18`.

## 4. Expected value per engineering day
FACT — No source states a dollar EV-per-engineering-day number; the canonical artifact ranks moves by EV/engineering-day only. `.agent-private/alpha-source-investigation/TOP_3_REVENUE_MOVES.md:1,52-57`.
FACT — Ranking: 1) MOVE 1 (SVR lane) — highest ceiling, existing code, no new infra; 2) MOVE 2 (event trigger) — prerequisite for 1&3, fixes the structural recall killer; 3) MOVE 3 (universe) — captures proven small wins. Recommend 1+2 together, 3 after. `TOP_3_REVENUE_MOVES.md:52-57`.
FACT — Cost/time/net: Move 1 LOW-MEDIUM, 7 days shadow, 2-4 wks to prod, net/wk UNKNOWN; Move 2 MEDIUM, 3-6 wks, ESTIMATED NET $2-10/wk; Move 3 MEDIUM, 14 days shadow, 4-8 wks, ESTIMATED NET $50-150/wk. `TOP_3_REVENUE_MOVES.md:15-19,31-35,44-48`.
FACT — Honest overall: "verified direct wins are $5-20/week class; route expansion ≈ $50-150/week net; the SVR lane size is UNKNOWN until the 7-day shadow validation. No guarantee." `FINAL_VERDICT.md:46-48`.
UNKNOWN — Numeric $/engineering-day EV not derivable from sources; only ranking + per-week net estimates exist.

## 5. Funnel conversion
FACT — SVR/Atlas intake funnel: 24,709 auctions ingested / 7d (~147/h); ~57% Aave-relevant; ALL economic_rejection (atlas_callback_evidence_unavailable); 0 candidate signals; 0 eligible; 0 bids. `SVR_ATLAS_REALITY.md:6,14-15`.
FACT — Pre-fix shadow classification: 100% `atlas_auction_bounds_invalid` (1,779/1,779) — bounds gate applied DIRECT-lane caps (600k gas / ~50 gwei) to SOLVER-side fields (live solver gas up to 6,000,000; oracle prices 38-621 gwei). `.agent-private/revenue-capture-program/03_SHADOW_VALIDATION_LOG.md:17-34`.
FACT — Post-fix (Release #2, 2026-08-21 ~05:33Z): 19/19 shadow-evaluated → `superseded_without_evaluation`, 0 `bounds_invalid` (vs 100% pre-fix); BID_ABILITY still 0 — queue draining slower than ~4/min auction stream. `03_SHADOW_VALIDATION_LOG.md:90-103`.
FACT — Aave direct-lane funnel: 186,426-borrower seed (complete through 2026-08-05) → 69,616 debt-bearing → 70,016 distinct checked/7d → 4,117 liquidatable borrowers/7d → 0/4,556 forks pass → $0 realized. `.agent-private/alpha-source-investigation/SIGNAL_RECALL_SCORECARD.md:9-17`; `.agent-private/alpha-source-investigation/ASSET_UNIVERSE_GAP.md:5`.
FACT — Ground-truth join (first collection, window 496,777,653..496,795,657, 2026-08-21 ~08:05Z): reconciled=538, settlements=13, public_liquidations=3, rows_loaded=3; liquidations_total=3, weth_debt_total=0, non_weth_debt=3; with_ingress=3, with_shadow_evaluation=3, shadow_eligible=0. `.agent-private/revenue-capture-program/04_PROGRAM_LOG.md:218-229`.
FACT — Recall scorecard (n=2): discovery 2/2, liquidatable-signal 2/2, oracle-event UNKNOWN (structural 0), exact-evaluation 0/2, addressable 0/2, execution-capable 0/2, asset-supported 0/2, route-available 0/2. `.agent-private/alpha-source-investigation/ACTUAL_LIQUIDATION_GROUND_TRUTH.md:28-39`.

## 6. Asset / route / size opportunity
FACT — Reviewed universe: WETH-debt positions + WETH_IDENTITY unwind (WETH/USDC Uniswap V3 fee tiers 500/3000); A1 engine enumerates 2 directed cycles, 1 SHADOW-enabled. `ASSET_UNIVERSE_GAP.md:4`; `docs/AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md:42-45`.
FACT — Excluded direct universe: 0.73 ETH/wk gross ceiling = no_weth_debt 0.303 ETH (5,296 sigs, 589 borrowers) + other exclusions 0.423 ETH (12,344 sigs). `ASSET_UNIVERSE_GAP.md:5-7`.
FACT — SVR auction oracle-asset distribution (58,227 ingress; 34,097 Aave-relevant, 2026-08-20T22:40Z): ETH 12,034 (35.3%), ARB 11,056 (32.4%), BTC 8,632 (25.3%), LINK 1,880 (5.5%), AAVE 334 (1.0%). `.agent-private/revenue-capture-program/05_WORKSTREAM_C_NON_WETH_SHADOW_PAIR.md:10-21`.
FACT — ARB = evidence-selected non-WETH shadow pair (11,056 Aave-relevant ARB auctions = 32.4%, statistically indistinguishable from WETH 35.3%). `05_WORKSTREAM_C_NON_WETH_SHADOW_PAIR.md:23-32`.
FACT — Both confirmed external winners were non-WETH-debt positions with no reviewed unwind route. `ASSET_UNIVERSE_GAP.md:8,12-18`.
FACT — Whale: 0x04511e23 liquidatable since ~Aug 13, ub 4.3-5.3e15 wei ≈ $10-12 gross at $2,263/ETH ("~$18" was at old $3,400 ETH); collateral zeroed 2026-08-20 11:25-12:20 UTC; non-recurring. `ASSET_UNIVERSE_GAP.md:20-29`.

## 7. Expected vs conservative vs realized PnL
FACT — Realized Net PnL = $0 at every as-of date: `realized_net_pnl: 0`, empty entries (2026-08-02); program-wide 0 attempts/submissions/solver requests at every sample (2026-08-20→21). `docs/evidence/platform-transition-20260802/ECONOMIC_LEDGER.json:9-10`; `03_SHADOW_VALIDATION_LOG.md:57-60,126-129`.
FACT — Expected/modeled shadow PnL is wei-only and NEVER converted to Realized: AUCTION_VALUE_PROXY = SUM(conservative_net_after_bid_wei) + SUM(expected_net_after_bid_wei); currently 0/0 wei (no eligible rows). `.agent-private/revenue-capture-program/02_SHADOW_VALIDATION_PLAN.md:29-32`; `03_SHADOW_VALIDATION_LOG.md:13,40`.
FACT — Conservative direct-lane formula: gross profit − flash premium − gas − ordering-cost reserve − model-error reserve; only strictly >0 AND above retained-profit floor becomes a candidate. `docs/AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md:87-95`.
FACT — Weekly EXPECTED (modeled) net, ESTIMATED: Move 2 event trigger $2-10/wk; Move 3 universe $50-150/wk; route expansion $100-400/wk net (worst-case-dominant); SVR lane UNKNOWN. `TOP_3_REVENUE_MOVES.md:32,46`; `MISSING_ALPHA_LEDGER.md:17`.
FACT — Weekly CONSERVATIVE bound: route expansion 0.73 ETH/wk gross ceiling (~$1.65k/wk at $2,263/ETH), "mostly one non-recurring whale". `MISSING_ALPHA_LEDGER.md:17`.
UNKNOWN — Conservative net for the SVR lane until shadow bid economics exist; `realization_status: "not realized; SHADOW evidence only"` at every sample. `03_SHADOW_VALIDATION_LOG.md:59-60`.

## 8. Competitor / missed-opportunity evidence
FACT — On-chain winner identities could NOT be resolved at scale (public RPC limits; winning txs route internally via flashloan/solver contracts) → UNKNOWN_DUE_TO_DATA_LIMIT for addresses. `.agent-private/alpha-source-investigation/COMPETITOR_SCOREBOARD.md:3-5`.
FACT — Proven winner behavior: (1) winners took positions Phoenix observed liquidatable for HOURS/DAYS which Phoenix policy-excluded (non-WETH debt) → BROADER ASSET UNIVERSE; (2) winners ignore dust Phoenix burns fork budget on → COST-AWARE SELECTIVITY; (3) 147 auctions/h, ~57% Aave-relevant — Phoenix bids in ZERO; (4) SVR auctions arrive ~0.5s after oracle update — winners' edge is event-driven detection, not superior feed access. `COMPETITOR_SCOREBOARD.md:8-19`.
FACT — "WHAT WINNERS DO THAT PHOENIX DOES NOT" (ranked): 1) act on oracle events in seconds (Phoenix: ~31h sweep lag); 2) cover non-WETH debt/collateral pairs; 3) bid in SVR/Atlas auctions; 4) skip dust — Phoenix fork budget burns on ~$0.02 edges. `COMPETITOR_SCOREBOARD.md:34-38`.
FACT — Missed-opportunity ledger: 2026-08-20 ~$10-12 gross (non-WETH whale, Phoenix saw 46×/7d, captured 0); 2026-08-19 ~$0.27 gross (saw 8×, captured 0); 2026-08-13→20 SVR auction winners across ETH/ARB/BTC/LINK/AAVE (Phoenix ingested 14,142 relevant auctions, captured 0). `.agent-private/alpha-source-investigation/MISSING_ALPHA_LEDGER.md:8-10`.
FACT — Elite-stack gap matrix: MISSING on hot in-memory state, event-driven trigger, broad asset coverage, broad unwind routing, cost-selective evaluation; PARTIAL on feed tap + SVR participation; HAS best-in-class fork-verified simulation, fail-closed authority, economic control plane. `.agent-private/alpha-source-investigation/ELITE_SEARCHER_GAP_MATRIX.md:4-18`.
FACT — Ordering NOT the binding constraint: "the liquidation battle is decided in SECONDS after oracle updates (event detection), not in block-ordering auctions." Timeboost live; PGA pending. `.agent-private/alpha-source-investigation/ARBITRUM_ORDERING_REALITY.md:22-25`.

## 9. Next highest-value engineering move
FACT — Canonical next move: MOVE 1 — unblock the Atlas/SVR solver lane in SHADOW (bid evaluation on the real auction stream), then owner-gated live bidding with atlETH bonding. "The only channel with constant, current liquidation value." `TOP_3_REVENUE_MOVES.md:3-21`; `EXECUTIVE_REVENUE_VERDICT.md:15-18`.
FACT — Supporting moves: 2) event-driven screening (trigger from SVR/oracle events instead of the 31h sweep); 3) owner-reviewed non-WETH pair expansion. Execute 1+2 together, 3 after. `EXECUTIVE_REVENUE_VERDICT.md:16-22`; `TOP_3_REVENUE_MOVES.md:52-57`.
FACT — Shortest-path plan: 7-day shadow bid evaluation → gate on N positive fork-verified auctions at bid ≥ floor → owner gate (atlETH bonding, arming, bid ceiling) → live bid only when economics clear floor+costs+bid → reconcile first won auction as realized. `.agent-private/alpha-source-investigation/FIRST_POSITIVE_REALIZED_PNL_PLAN.md:7-21`.
FACT — Current program state (as-of 2026-08-21 ~08:05Z): Workstream B shadow classification live (bounds fix deployed Release #2/#5); Workstream A ground truth collected (3 liquidation rows); Workstream C ARB instrumentation merged; 7-day validation window open (T0 = 2026-08-20T21:18Z, ends 2026-08-27T21:18Z); effective bid-ability clock restarted at Release #2 05:33Z; BID_ABILITY=0 pending exact-evaluation queue drain. `03_SHADOW_VALIDATION_LOG.md:90-129`; `02_SHADOW_VALIDATION_PLAN.md:60-74`.
FACT — What NOT to do: "lower the floor, weaken reserves, chase cross-chain millisecond signals, buy express-lane ordering, or spend engineering on the unprofitable WETH/USDC DEX lane." `EXECUTIVE_REVENUE_VERDICT.md:24-27`.
HYPOTHESIS/PROPOSAL — "First positive PnL is plausible in weeks, not days, and its size is UNKNOWN until shadow data exists. We do not guarantee profit; we guarantee the highest-evidence path to it." `FIRST_POSITIVE_REALIZED_PNL_PLAN.md:29-32`.

## UNKNOWN / DATA GAPS
- SVR/Atlas Arbitrum PnL size and conservative net: UNKNOWN until 7-day shadow bid economics (`ADDRESSABLE_MARKET_PNL.md:10`; `TOP_3_REVENUE_MOVES.md:15`).
- On-chain winner identities at scale: UNKNOWN_DUE_TO_DATA_LIMIT (`COMPETITOR_SCOREBOARD.md:3-5`).
- Exact 24h/7d/30d LiquidationCall counts: UNKNOWN_DUE_TO_DATA_LIMIT (~10h sweep saw 0; 6/12 rate-limited chunks failed incl. critical window) (`ACTUAL_LIQUIDATION_GROUND_TRUTH.md:3-6,22-26`).
- Oracle-event recall: structural 0 — no oracle-event ingestion in the Aave lane (`ACTUAL_LIQUIDATION_GROUND_TRUTH.md:33`).
- Numeric $/engineering-day EV: not stated in any source (`TOP_3_REVENUE_MOVES.md`).
- SVR/Atlas-mediated winner side for the 2 confirmed wins: UNKNOWN (winner tx not retrievable) (`ASSET_UNIVERSE_GAP.md:27`).
- `.agent-private/alpha-source-investigation/ground-truth/` is EMPTY — no representative file to read (gap in the requested reading list).
- A1 fixture live-fork prediction-error: explicitly null (synthetic fixture, not production) (`AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md:120-123`).

## EVIDENCE SOURCES
- `.agent-private/alpha-source-investigation/EXECUTIVE_REVENUE_VERDICT.md`
- `.agent-private/alpha-source-investigation/MISSING_ALPHA_LEDGER.md`
- `.agent-private/alpha-source-investigation/ACTUAL_LIQUIDATION_GROUND_TRUTH.md`
- `.agent-private/alpha-source-investigation/TOP_3_REVENUE_MOVES.md`
- `.agent-private/alpha-source-investigation/FINAL_VERDICT.md`
- `.agent-private/alpha-source-investigation/ADDRESSABLE_MARKET_PNL.md`
- `.agent-private/alpha-source-investigation/COMPETITOR_SCOREBOARD.md`
- `.agent-private/alpha-source-investigation/ELITE_SEARCHER_GAP_MATRIX.md`
- `.agent-private/alpha-source-investigation/SVR_ATLAS_REALITY.md`
- `.agent-private/alpha-source-investigation/SIGNAL_RECALL_SCORECARD.md`
- `.agent-private/alpha-source-investigation/ASSET_UNIVERSE_GAP.md`
- `.agent-private/alpha-source-investigation/ARBITRUM_ORDERING_REALITY.md`
- `.agent-private/alpha-source-investigation/FIRST_POSITIVE_REALIZED_PNL_PLAN.md`
- `.agent-private/alpha-source-investigation/ground-truth/` (EMPTY)
- `.agent-private/revenue-capture-program/00_HANDOFF.md`
- `.agent-private/revenue-capture-program/00_BASELINE.md`
- `.agent-private/revenue-capture-program/01_WORKSTREAM_B_ATLAS_SHADOW_SOLVER.md`
- `.agent-private/revenue-capture-program/02_SHADOW_VALIDATION_PLAN.md`
- `.agent-private/revenue-capture-program/03_SHADOW_VALIDATION_LOG.md`
- `.agent-private/revenue-capture-program/04_PROGRAM_LOG.md`
- `.agent-private/revenue-capture-program/05_WORKSTREAM_C_NON_WETH_SHADOW_PAIR.md`
- `.agent-private/revenue-capture-program/06_FINAL_CLASSIFICATION_RUBRIC.md`
- `.agent-private/revenue-capture-program/07_GROUND_TRUTH_COLLECTION_PLAN.md`
- `.agent-private/revenue-capture-program/08_POST_RELEASE_HARD_GATE.md`
- `docs/AUTONOMOUS_HUNTER_A1_REVENUE_EVIDENCE.md`
- `docs/evidence/platform-transition-20260802/ECONOMIC_LEDGER.json`
- `docs/evidence/platform-transition-20260802/DECISION_LEDGER.ndjson`
