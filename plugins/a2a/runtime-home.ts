// Where the Rust side keeps its runtime files (tabs.jsonl, codetypera2a.path). Pure: no opencode imports.
// Order: explicit variable, then the runtime home (`$CODETYPERA2A_HOME` or `~/.codetypera2a`), then the
// legacy `<config>/a2a` folder, used only when the new file is absent and the legacy one exists.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export type Env = Record<string, string | undefined>;

export interface Ctx {
  env: Env;
  home: string;
  config: string;
  exists: (p: string) => boolean;
}

const BIN_FALLBACK = "codetypera2a";

const canonicalFile = (name: string, c: Ctx): string => {
  if (name === "tabs.jsonl" && c.env.A2A_TABS) return c.env.A2A_TABS;
  if (c.env.A2A_DB) return path.join(path.dirname(c.env.A2A_DB), name);
  return path.join(c.env.CODETYPERA2A_HOME || path.join(c.home, ".codetypera2a"), name);
};

export const runtimeFile = (name: string, c: Ctx): string => {
  const canonical = canonicalFile(name, c);
  const legacy = path.join(c.config, "a2a", name);
  if (c.exists(canonical) || !c.exists(legacy)) return canonical;
  return legacy;
};

/** The binary `init` recorded, else `codetypera2a` from PATH. `read` returns the file text or throws. */
export const binPath = (c: Ctx, read: (p: string) => string): string => {
  try {
    return read(runtimeFile("codetypera2a.path", c)).trim() || BIN_FALLBACK;
  } catch {
    return BIN_FALLBACK;
  }
};

/** The opencode config folder (agent files live in `agents/`). */
export const configDir = (): string =>
  process.env.OPENCODE_CONFIG_DIR || path.join(os.homedir(), ".config", "opencode");

export const liveCtx = (): Ctx => ({
  env: process.env,
  home: os.homedir(),
  config: configDir(),
  exists: fs.existsSync,
});

export const tabsFile = (): string => runtimeFile("tabs.jsonl", liveCtx());

export const a2aBin = (): string => binPath(liveCtx(), (p) => fs.readFileSync(p, "utf8"));
