---
schema: phoenix.silver-bullet-baseline-markdown.v1
generated_at: 2026-08-02T07:41:24.293Z
repository_sha: aa4ab66de054b4a1ab0966d78923b78ab789ded1
active_release_sha: c40ad821d564b30bf43d31d608669999f5e03efc
source_identity: release-run-30707220498
completeness: complete_for_platform_failure_reconstruction
unknowns: protected release and current economic refresh remain intentionally unrun
---

# Phoenix platform baseline

Run `30707220498` packaged a seven-image inherited candidate at source SHA
`aa4ab66de054b4a1ab0966d78923b78ab789ded1`. Render validation excluded
Atlas, deploy did not start Atlas, but running-image validation independently
demanded it and failed with `RUNNING_IMAGE_MISMATCH`,
`service=atlas-observer`, `stage=container-missing`. Automatic rollback restored
`c40ad821d564b30bf43d31d608669999f5e03efc`.

The candidate manifest and context remained bound throughout the failing path;
there is no evidence that candidate, active, and rollback contexts were mixed.
The active safety posture was SHADOW, live execution false, autonomous execution
false, armed false, kill switch true, and contract paused.

The seven-image bridge is structurally required: the pre-bridge gateway accepts
only seven images, while the installed bridge-capable gateway accepts seven or
eight. Stage A must therefore activate a seven-image package before Stage B can
activate the independently bound eight-image package.
