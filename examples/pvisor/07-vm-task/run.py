"""Framework-side example: submit a task and consume observations from a worker."""

import json
from pathlib import Path

from executor import ExecutionError, execute

EXAMPLE = Path(__file__).resolve().parent


def main():
    task = json.loads((EXAMPLE / "task.json").read_text())
    try:
        feedback = execute(task, EXAMPLE / "candidate.patch")
    except ExecutionError as error:
        # A real framework can retry or report an infrastructure error here.
        raise SystemExit(f"Execution error: {error}") from error

    # Return these observations to your Agent, evaluator or rollout collector.
    for step in feedback["steps"]:
        print(f"Step {step['step']}: exit_code={step['exit_code']}, timed_out={step['timed_out']}")
        print(step["stdout"].strip())
        if step["stderr"]:
            print(step["stderr"].strip())
    print(f"Job: {feedback['job_id']}; Attempt: {feedback['attempt_id']}")
    print(f"Results: {Path(feedback['records_dir']) / 'sample.json'}")
    if any(s["exit_code"] != 0 or s["timed_out"] for s in feedback["steps"]):
        raise SystemExit("Tool feedback contains a failure; see the retained records")


if __name__ == "__main__":
    main()
