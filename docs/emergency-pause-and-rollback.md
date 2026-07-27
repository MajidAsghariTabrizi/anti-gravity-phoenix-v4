# Emergency Pause and Rollback

`emergency-pause` and `rollback <sha>` use the same forced-command gateway as normal
releases. They are break-glass operations and produce bounded JSON evidence.

## Automatic failure handling

Before mutation, a failed check records `FAILED_PRE_MUTATION`, leaves the active
runtime untouched, keeps the executor paused/stopped, and preserves active pointers.

After mutation, the controller:

1. stops live-executor;
2. proves or restores contract pause;
3. activates the kill switch and disarms autonomous execution;
4. restores SHADOW mode;
5. installs the version-matched rollback context and images;
6. restores coherent pointers;
7. verifies protected service identity plus RPC Gateway and Engine health;
8. records `ROLLED_BACK`.

If any proof fails, every money-path component remains stopped, pause/kill-switch
controls remain fail-closed, and state becomes `ROLLBACK_FAILED` with exact evidence.

## Manual emergency protocol

`emergency-pause` stops live-executor, executes the allowlisted owner-pause control,
and restores SHADOW flags. `rollback <sha>` accepts only the immutable
`previous-release` identity and version-matched scripts. Arbitrary targets and shell
commands are rejected.

After either command, inspect `status`, `history`, and `evidence <sha>`. Never retry
an owner operation when state is `EXECUTOR_UNPAUSE_STARTED`; first reconcile the
known transaction hash, receipt, latest nonce, pending nonce, and contract state.
