import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  isNewerVersion,
  maybeAutoUpdate,
  updateCommand,
} from "./codex-agents-update.js";

test("compares stable and prerelease versions", () => {
  assert.equal(isNewerVersion("1.2.4", "1.2.3"), true);
  assert.equal(isNewerVersion("1.2.3", "1.2.3"), false);
  assert.equal(isNewerVersion("1.2.3", "1.2.3-beta.1"), true);
  assert.equal(isNewerVersion("1.2.3-beta.2", "1.2.3-beta.10"), false);
});

test("builds self-update commands for supported package managers", () => {
  assert.deepEqual(updateCommand("npm", "1.2.3"), [
    "npm",
    ["install", "--global", "codex-agents@1.2.3"],
  ]);
  assert.deepEqual(updateCommand("pnpm", "1.2.3"), [
    "pnpm",
    ["add", "--global", "codex-agents@1.2.3"],
  ]);
  assert.deepEqual(updateCommand("bun", "1.2.3"), [
    "bun",
    ["install", "--global", "codex-agents@1.2.3"],
  ]);
});

test("detects and installs an available update", async () => {
  const calls = [];
  const cacheDir = mkdtempSync(path.join(os.tmpdir(), "codex-agents-update-"));
  const updated = await maybeAutoUpdate({
    packageName: "codex-agents",
    currentVersion: "1.0.0",
    packageManager: "npm",
    env: { CODEX_AGENTS_UPDATE_CACHE: path.join(cacheDir, "cache.json") },
    now: 123_000,
    run(command, args) {
      calls.push([command, args]);
      if (args[0] === "view") {
        return { status: 0, stdout: '"1.1.0"\n' };
      }
      return { status: 0 };
    },
  });

  assert.equal(updated, true);
  assert.deepEqual(calls, [
    ["npm", ["view", "codex-agents", "version", "--json"]],
    ["npm", ["install", "--global", "codex-agents@1.1.0"]],
  ]);
});

test("does not update the upstream Codex package", async () => {
  const updated = await maybeAutoUpdate({
    packageName: "@openai/codex",
    currentVersion: "1.0.0",
    packageManager: "npm",
    run() {
      throw new Error("registry should not be queried");
    },
  });

  assert.equal(updated, false);
});
