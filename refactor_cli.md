当前的命令行接口如下：
```
❯ ./target/release/pvisor -h
Run agents, review changes, and manage Jobs

Usage: pvisor [OPTIONS] <COMMAND>

Execution:
  run         Run an agent or command
  status      Show Job status and execution evidence
  kill        Stop a running Job

Changes:
  review      Review staged changes and execution evidence
  apply       Apply reviewed changes
  drop        Discard staged changes
  inspect     Inspect a Job's filesystem

Checkpoints:
  checkpoint  Manage Job checkpoints
  suspend     Suspend a Job or pause a live VM
  resume      Resume a suspended Job or live VM
  fork        Branch a Job from staged files or a checkpoint

Tools:
  docker      Run Docker-compatible commands in independent rootless VMs
  cache       Build, publish and query image, rootfs and workspace caches
  replay      Replay an agent-native trajectory
  tui         Run a Job in an interactive terminal
  feature     List runtime experiments
  help        Print this message or the help of the given subcommand(s)

Options:
      --feature <NAME>  Enable a runtime experiment (repeatable, comma-separated)
  -h, --help            Print help
  -V, --version         Print version

Examples:
  pvisor run --safe -- claude
  pvisor review last
  pvisor apply last --path src

Use pvisor -- COMMAND for default execution.
Use pvisor help COMMAND for command details.
```

我觉得应该重构成以docker兼容命令为骨架的形式

```
❯ ./target/release/pvisor
Usage: pvisor [OPTIONS] <COMMAND>

Commands:
  run     Create and run a new container in its own VM
  spawn   Run an agent or command as a job
  status  Show Job status and execution evidence
  pull    Download an image from an OCI registry
  images  List locally pulled images
  rmi     Remove one or more local image references
  exec    Execute a command inside a running container VM
  ps      List containers
  start   Start one or more stopped containers, retaining their writable files
  stop    Stop one or more running containers
  rm      Remove one or more containers
  logs    Fetch container stdout and stderr
  help    Print this message or the help of the given subcommand(s)

changes Commands：
  review      Review staged changes and execution evidence
  apply       Apply reviewed changes
  drop        Discard staged changes
  inspect     Inspect a Job's filesystem

checkpoints：
  checkpoint  Manage Job checkpoints
  suspend     Suspend a Job or pause a live VM
  resume      Resume a suspended Job or live VM
  fork        Branch a Job from staged files or a checkpoint

Tools:
  cache       Build, publish and query image, rootfs and workspace caches
  replay      Replay an agent-native trajectory
  tui         Run a Job in an interactive terminal
  feature     List runtime experiments
  help        Print this message or the help of the given subcommand(s)

Options:
      --feature <NAME>  Enable a runtime experiment (repeatable, comma-separated)
  -h, --help            Print help
```
