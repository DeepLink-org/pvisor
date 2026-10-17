Frozen SWE tool trajectories — data-only review supplement for PR #29

30 existing SWE-bench Pro task trajectories: 10 NodeBB, 10 Vuls and 10
qutebrowser; 2,486 Agent tool commands. Source runs used SweEval 0.1.0
(SweEval-1e31b894) and mini-swe-agent 2.4.6. No new execution was performed
for this data review. This directory contains data and documentation only.

Each task JSON preserves the command, cwd, user, explicit environment and
original timeout; relative host-monotonic start/end timestamps; inter-tool
and trailing waits; API request intervals; expected exit/timeout outcomes;
stdout/stderr SHA-256; final patch SHA-256; pinned public image, Git base,
VM settings and initial Git fingerprints. setup commands precede Agent
steps and are not included in the 2,486 Agent-command count. manifest.json
fixes every task file by SHA-256 and byte count.

No model messages/reasoning text, raw stdout/stderr, full final patches,
API configuration/keys, handshake tokens, absolute host times or paths,
rootfs or RAM are included. Commands retain source edits and excerpts and
have not been rewritten to manufacture matching results. Credentials-like
keywords in retained commands refer to application variables/test code.
The full inherited image environment is represented only by its hash.
Raw logs and full patches remain in the private retained source experiment.

Timing for reuse
- fast: omit recorded inter-tool and trailing waits; execute tools normally.
- historical: wait the recorded gap after each preceding tool actually
  finishes, including the final idle gap. Tool runtime is measured anew.
Recorded gaps total 7,842.107578341 s; API intervals total 7,700.583540618 s.
API intervals combine queueing, inference/generation, network and failures;
they do not isolate reasoning. No model API is needed to consume commands.
This data-only supplement does not add or change a replay implementation.
It is not a complete standalone runtime or rootfs distribution.

Validation and known limits
The current read-only audit checked all 30 records against retained source
commands, events, raw output hashes and final patches. data-audit.json
records those checks. SHA-256 and license mappings were rechecked locally.
validation-summary.json is the unchanged HISTORICAL replay evidence, not a
new run: all 30 fast sequences executed; 22 matched the old combined
comparison/finalization criterion, 4 had tool/patch differences and 4 had
finalization failures. 29/30 final patches matched. Three historical-wait
representatives passed; the other 27 had only offline schedule checks.
Output byte drift is reported separately from exit/patch differences.
These failures are retained, not relabeled as successful durable VM exits.
They do not erase already-recorded commands, timestamps, outputs or patches.
This is not official SWE scoring, a deterministic golden set or proof of
pause/offload/resume performance. Downloads and timeouts remain variable.

Attribution
Source excerpts retain their original project licenses; this supplement
does not relicense them. licenses/manifest.json maps every task to the
exact retained LICENSE text (four distinct files with SHA-256 hashes).
NodeBB: https://github.com/NodeBB/NodeBB (GPL-3.0)
Vuls: https://github.com/future-architect/vuls (GPL-3.0 or AGPL-3.0 by revision)
qutebrowser: https://github.com/qutebrowser/qutebrowser (GPL-3.0)
Use origin.task_id and origin.base_commit for exact task/revision identity.
Images and the original SweEval task packages are obtained separately.

Review candidate only; nothing has been uploaded or pushed to GitHub.
