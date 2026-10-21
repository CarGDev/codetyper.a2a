// a2a tabs: shows every agent's session as a tab in this opencode.
//
// The A2A dispatcher gives each agent its own session (one per agent per project) and appends a
// line to `tabs.jsonl` next to the hub database whenever it hands that agent work:
//   {"sessionID":"ses_...","agent":"mentor","directory":"/path/to/project","at":"..."}
// This plugin tails that file and opens the session as a tab (`context.ui.tabs.open`, which does
// not steal focus) when the project matches the folder opencode was started in.
// Docs: https://opencode.ai/v2/docs/build/plugins/cli (Plugin.define, context.ui.tabs).
import { Plugin } from "@opencode/plugin/tui";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";

const tabsFile = (): string => {
  const config = configDir();
  const db = process.env.A2A_DB || path.join(config, "a2a", "hub.db");
  return path.join(path.dirname(db), "tabs.jsonl");
};

const configDir = (): string => {
  return (
    process.env.OPENCODE_CONFIG_DIR ||
    path.join(os.homedir(), ".config", "opencode")
  );
};

// The binary `init` recorded (it may not be on opencode's PATH), else `codetypera2a` from PATH.
const a2aBin = (): string => {
  try {
    const p = fs
      .readFileSync(path.join(configDir(), "a2a", "codetypera2a.path"), "utf8")
      .trim();
    if (p) return p;
  } catch {
    // not recorded: fall back to PATH
  }
  return "codetypera2a";
};

const sameProject = (dir: string, cwd: string): boolean => {
  return (
    cwd === dir ||
    cwd.startsWith(dir + path.sep) ||
    dir.startsWith(cwd + path.sep)
  );
};

export default Plugin.define({
  id: "a2a.tabs",
  setup(context: any) {
    // Opening opencode is the trigger: make sure the dispatcher runs and, inside tmux (this process
    // inherits TMUX_PANE), that the live `a2a-status` tab is open. Detached, output ignored.
    try {
      // `--client` counts this TUI as a running opencode; the cleanup below ends everything when the
      // last one exits
      const child = spawn(
        a2aBin(),
        ["oc", "ensure", "--client", String(process.pid)],
        { stdio: "ignore", detached: true },
      );
      child.on("error", () => {});
      child.unref();
    } catch {
      // never break the TUI over this
    }

    const file = tabsFile();
    // Only react to lines written after this TUI started.
    let offset = 0;
    try {
      offset = fs.statSync(file).size;
    } catch {
      offset = 0;
    }

    const poll = () => {
      try {
        if (!context.ui.tabs.enabled()) {
          return;
        }
        const size = fs.statSync(file).size;
        if (size < offset) {
          offset = 0; // file was truncated
        }
        if (size === offset) {
          return;
        }
        const fd = fs.openSync(file, "r");
        const buf = Buffer.alloc(size - offset);
        fs.readSync(fd, buf, 0, buf.length, offset);
        fs.closeSync(fd);
        const text = buf.toString("utf8");
        const end = text.lastIndexOf("\n");
        if (end < 0) {
          return; // incomplete line, wait for the rest
        }
        offset += Buffer.byteLength(text.slice(0, end + 1));
        for (const line of text.slice(0, end).split("\n")) {
          if (!line.trim()) {
            continue;
          }
          let msg: any;
          try {
            msg = JSON.parse(line);
          } catch {
            continue;
          }
          if (
            !msg.sessionID ||
            !msg.directory ||
            !sameProject(String(msg.directory), process.cwd())
          ) {
            continue;
          }
          context.ui.tabs.open(msg.sessionID);
          context.ui.toast.show({
            message: `a2a: ${msg.agent ?? "agent"} tab opened`,
            variant: "info",
          });
        }
      } catch {
        // file not there yet, or a transient read error: try again on the next tick
      }
    };

    const timer = setInterval(poll, 500);
    return () => {
      clearInterval(timer);
      // opencode is closing: cancel open tasks, stop running turns and the dispatcher, unless another
      // opencode is still running (A2A_KEEP_RUNNING=1 keeps everything)
      try {
        const child = spawn(
          a2aBin(),
          ["oc", "shutdown", "--client", String(process.pid)],
          { stdio: "ignore", detached: true },
        );
        child.on("error", () => {});
        child.unref();
      } catch {
        // never break the exit
      }
    };
  },
});
