import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";

const AUTO_UPDATE_PACKAGE = "codex-agents";
const CHECK_INTERVAL_MS = 20 * 60 * 60 * 1000;

function parseVersion(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/.exec(version);
  if (!match) {
    return null;
  }
  return {
    core: match.slice(1, 4).map(Number),
    prerelease: match[4]?.split(".") ?? [],
  };
}

export function isNewerVersion(candidate, current) {
  const candidateVersion = parseVersion(candidate);
  const currentVersion = parseVersion(current);
  if (!candidateVersion || !currentVersion) {
    return false;
  }

  for (let index = 0; index < candidateVersion.core.length; index += 1) {
    if (candidateVersion.core[index] !== currentVersion.core[index]) {
      return candidateVersion.core[index] > currentVersion.core[index];
    }
  }

  if (candidateVersion.prerelease.length === 0) {
    return currentVersion.prerelease.length > 0;
  }
  if (currentVersion.prerelease.length === 0) {
    return false;
  }

  const length = Math.max(
    candidateVersion.prerelease.length,
    currentVersion.prerelease.length,
  );
  for (let index = 0; index < length; index += 1) {
    const candidatePart = candidateVersion.prerelease[index];
    const currentPart = currentVersion.prerelease[index];
    if (candidatePart === currentPart) {
      continue;
    }
    if (candidatePart === undefined) {
      return false;
    }
    if (currentPart === undefined) {
      return true;
    }
    const candidateNumber = /^\d+$/.test(candidatePart)
      ? Number(candidatePart)
      : null;
    const currentNumber = /^\d+$/.test(currentPart) ? Number(currentPart) : null;
    if (candidateNumber !== null && currentNumber !== null) {
      return candidateNumber > currentNumber;
    }
    if (candidateNumber !== null) {
      return false;
    }
    if (currentNumber !== null) {
      return true;
    }
    return candidatePart > currentPart;
  }
  return false;
}

export function updateCommand(packageManager, version) {
  const spec = `${AUTO_UPDATE_PACKAGE}@${version}`;
  switch (packageManager) {
    case "bun":
      return ["bun", ["install", "--global", spec]];
    case "pnpm":
      return ["pnpm", ["add", "--global", spec]];
    default:
      return ["npm", ["install", "--global", spec]];
  }
}

function readCachedVersion(cachePath, now) {
  try {
    const cache = JSON.parse(readFileSync(cachePath, "utf8"));
    if (
      typeof cache.latestVersion === "string" &&
      typeof cache.lastCheckedAt === "number" &&
      now - cache.lastCheckedAt < CHECK_INTERVAL_MS
    ) {
      return cache.latestVersion;
    }
  } catch {
    // A missing or invalid cache should trigger a fresh registry check.
  }
  return null;
}

function fetchLatestVersion(packageManager, cachePath, now, run) {
  const registryCommand = packageManager === "bun" ? "npm" : packageManager;
  const result = run(
    registryCommand || "npm",
    ["view", AUTO_UPDATE_PACKAGE, "version", "--json"],
    {
      encoding: "utf8",
      env: { ...process.env, NPM_CONFIG_UPDATE_NOTIFIER: "false" },
      timeout: 10_000,
    },
  );
  if (result.status !== 0) {
    return null;
  }

  try {
    const latestVersion = JSON.parse(result.stdout.trim());
    if (typeof latestVersion !== "string" || !parseVersion(latestVersion)) {
      return null;
    }
    mkdirSync(path.dirname(cachePath), { recursive: true });
    writeFileSync(
      cachePath,
      `${JSON.stringify({ latestVersion, lastCheckedAt: now })}\n`,
    );
    return latestVersion;
  } catch {
    return null;
  }
}

export async function maybeAutoUpdate({
  packageName,
  currentVersion,
  packageManager,
  env = process.env,
  now = Date.now(),
  run = spawnSync,
}) {
  if (
    packageName !== AUTO_UPDATE_PACKAGE ||
    env.CODEX_AGENTS_SKIP_AUTO_UPDATE === "1"
  ) {
    return false;
  }

  const cachePath =
    env.CODEX_AGENTS_UPDATE_CACHE ||
    path.join(os.homedir(), ".codex", "codex-agents-update.json");
  const latestVersion =
    readCachedVersion(cachePath, now) ||
    fetchLatestVersion(packageManager, cachePath, now, run);
  if (!latestVersion || !isNewerVersion(latestVersion, currentVersion)) {
    return false;
  }

  const [command, args] = updateCommand(packageManager, latestVersion);
  console.error(
    `Updating ${AUTO_UPDATE_PACKAGE} ${currentVersion} -> ${latestVersion}...`,
  );
  const result = run(command, args, { stdio: "inherit", env });
  if (result.status !== 0) {
    console.error(
      `Automatic update failed; continuing with ${AUTO_UPDATE_PACKAGE} ${currentVersion}.`,
    );
    return false;
  }
  return true;
}
