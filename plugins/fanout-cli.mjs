#!/usr/bin/env node
import { open } from "node:fs/promises";
import { constants } from "node:fs";
import { Backend } from "./backend.mjs";
import { fanout } from "./fanout.mjs";

const args = process.argv.slice(2);
if (args.length !== 4 || args[0] !== "--project" || args[2] !== "--input") {
  console.error("Usage: pvisor-agent-fanout --project DIR --input experiments.json");
  process.exitCode = 2;
} else {
  const controller = new AbortController();
  const stop = () => controller.abort();
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  try {
    const file = await open(args[3], constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    let input;
    try {
      if (!(await file.stat()).isFile()) throw new Error("Experiment plan must be a regular file");
      const bytes = Buffer.alloc(64 * 1024 + 1);
      let total = 0;
      while (total < bytes.length) {
        const { bytesRead } = await file.read(bytes, total, bytes.length - total, total);
        if (bytesRead === 0) break;
        total += bytesRead;
      }
      if (total > 64 * 1024) throw new Error("Experiment input exceeds 64 KiB");
      input = bytes.subarray(0, total).toString("utf8");
    } finally {
      await file.close();
    }
    const result = await fanout(new Backend(args[1]), JSON.parse(input), controller.signal);
    console.log(JSON.stringify(result));
    process.exitCode = result.exit_code;
  } catch (error) {
    console.error(String(error));
    process.exitCode = 1;
  } finally {
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
  }
}
