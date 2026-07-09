## Summary

## Why

## Scope

## Risk

## Hot Path Impact

## External RPC Impact

## State Correctness Impact

## Profit Accounting Impact

## Protocol Registry Impact

## Contract / Security Impact

## Tests Run

## Shadow Evidence

## Rollback Plan

## Mandatory Checklist

- [ ] I did not add an external public RPC read to the hot path.
- [ ] I did not silently change expected or realized PnL accounting.
- [ ] Opportunity route details remain immutable after optimization.
- [ ] I did not reconstruct the execution route independently.
- [ ] Protocol address / ABI changes are verified and documented.
- [ ] PhoenixExecutor changes have security tests.
- [ ] This change cannot automatically enable LIVE execution.
- [ ] No real secret is included in Git, logs, fixtures, or examples.
