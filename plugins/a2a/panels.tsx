/** @jsxImportSource @opentui/solid */
// a2a panels: the A2A hub drawn inside opencode.
//   - sidebar:  task table with a progress bar, one colour per agent, hop-cap marker
//   - composer: banner when a task waits for you or a context hit the hop cap
//   - footer:   dispatcher up/down, open tasks, today's estimated cost
//   - sounds:   system notification + sound when a task needs you, finishes or fails
//   - commands: /a2a-answer (pick an option for an `input-required` task), /a2a-continue, /a2a-cancel
// Data comes from `oc status --json`; actions run `oc answer|continue|cancel`.
// Loaded by tui.ts through a dynamic import, so a problem here never takes the tabs down.
// Docs: https://opencode.ai/v2/docs/build/plugins/cli (ui.slot, ui.dialog, keymap.layer, attention).
import { createSignal, For, Show } from "solid-js";
import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {
  barParts, capped, changes, EMPTY, frontmatterColor, glyph, hashColor, isOpen, progressLabel, statusLine, STATE_COLOR, waiting,
  type Snapshot, type Task,
} from "./panel-logic.ts";
import { a2aBin, configDir } from "./runtime-home.ts";

const MUTED = "#7f849c";
const WARN = "#e0af68";

const oc = (args: string[]): Promise<{ ok: boolean; out: string }> =>
  new Promise((resolve) => {
    execFile(a2aBin(), ["oc", ...args], { timeout: 8000 }, (err, stdout, stderr) =>
      resolve({ ok: !err, out: String(err ? stderr || err.message : stdout).trim() }),
    );
  });

const colors = new Map<string, string>();
const agentColor = (name: string): string => {
  let c = colors.get(name);
  if (!c) {
    try {
      c = frontmatterColor(fs.readFileSync(path.join(configDir(), "agents", `${name}.md`), "utf8"));
    } catch {
      // no agent file: hash below
    }
    c = c ?? hashColor(name);
    colors.set(name, c);
  }
  return c;
};

/** Registers everything; returns the cleanup. */
export function install(context: any): () => void {
  const [snap, setSnap] = createSignal<Snapshot>(EMPTY);
  const seen = new Map<string, string>();
  let busy = false;

  const notify = (title: string, message: string, sound: "question" | "done" | "error", variant: string) => {
    try {
      context.ui.toast.show({ title, message, variant, duration: 6000 });
      void context.attention.notify({
        title, message, notification: { when: "blurred" }, sound: { name: sound, when: "blurred" },
      });
    } catch {
      // cosmetic only
    }
  };

  const poll = async () => {
    if (busy) return;
    busy = true;
    try {
      const r = await oc(["status", "--json", "--dir", process.cwd()]);
      if (!r.ok) return;
      const s = JSON.parse(r.out) as Snapshot;
      setSnap(s);
      for (const c of changes(seen, s)) {
        const who = `${c.task.to} (${c.task.short})`;
        if (c.kind === "input") notify("a2a: needs you", `${who} is waiting. Run /a2a-answer`, "question", "warning");
        else if (c.kind === "done") notify("a2a: done", `${who} completed`, "done", "success");
        else if (c.kind === "failed") notify("a2a: failed", `${who} failed`, "error", "error");
        else notify("a2a: hop cap", `${c.task.context} hit the hop cap. Run /a2a-continue`, "question", "warning");
      }
    } catch {
      // bad JSON or a transient failure: try again on the next tick
    } finally {
      busy = false;
    }
  };
  void poll();
  const timer = setInterval(poll, 2000);

  // ---- commands ---------------------------------------------------------------------------------
  const pick = async (title: string, tasks: Task[]): Promise<Task | undefined> => {
    if (tasks.length <= 1) return tasks[0];
    const id = await context.ui.dialog.select({
      title,
      options: tasks.map((t) => ({ title: `${t.to} ${t.short}`, value: t.id, description: t.question?.text.split("\n")[0] ?? t.state })),
    });
    return tasks.find((t) => t.id === id);
  };

  const answer = async () => {
    const t = await pick("Which task?", waiting(snap()));
    if (!t) return context.ui.toast.show({ message: "a2a: no task is waiting for you", variant: "info" });
    const opts = t.question?.options ?? [];
    let text: string | undefined;
    if (opts.length) {
      const OTHER = "\u0000other";
      const v = await context.ui.dialog.select({
        title: `${t.to}: ${t.question?.text.split("\n")[0] ?? "needs a decision"}`,
        options: [
          ...opts.map((o, i) => ({ title: `${i + 1}) ${o}`, value: `${i + 1}) ${o}` })),
          { title: "Write my own answer…", value: OTHER },
        ],
      });
      if (v === undefined) return;
      text = v === OTHER ? undefined : v;
    }
    text = text ?? (await context.ui.dialog.prompt({ title: `Answer ${t.to}`, description: t.question?.text }));
    if (!text) return;
    const r = await oc(["answer", t.id, text]);
    context.ui.toast.show({ message: r.ok ? `a2a: ${r.out}` : `a2a: ${r.out}`, variant: r.ok ? "success" : "error" });
    void poll();
  };

  const cont = async () => {
    const t = await pick("Which context?", capped(snap()));
    if (!t) return context.ui.toast.show({ message: "a2a: no context hit the hop cap", variant: "info" });
    const r = await oc(["continue", t.context]);
    context.ui.toast.show({ message: `a2a: ${r.out.split("\n")[0]}`, variant: r.ok ? "success" : "error" });
    void poll();
  };

  const cancel = async () => {
    const t = await pick("Cancel which task?", snap().tasks.filter(isOpen));
    if (!t) return context.ui.toast.show({ message: "a2a: no open task", variant: "info" });
    const ok = await context.ui.dialog.confirm({
      title: "Cancel task", message: `${t.from} -> ${t.to} (${t.short}, ${t.state})`, label: { confirm: "Cancel task", cancel: "Keep" },
    });
    if (!ok) return;
    const r = await oc(["cancel", t.id, "--yes"]);
    context.ui.toast.show({ message: r.ok ? `a2a: canceled ${t.short}` : `a2a: ${r.out}`, variant: r.ok ? "success" : "error" });
    void poll();
  };

  // ---- views ------------------------------------------------------------------------------------
  const Sidebar = () => {
    const rows = () => snap().tasks.slice(-8);
    return (
      <Show when={snap().tasks.length > 0}>
        <box flexDirection="column">
          <text fg={MUTED}>a2a</text>
          <text>
            <span fg="#9a9a9a">{barParts(snap().progress.done, snap().progress.total)[0]}</span>
            <span fg={MUTED}>{barParts(snap().progress.done, snap().progress.total)[1]}</span>{" "}
            <span fg={MUTED}>{progressLabel(snap().progress.done, snap().progress.total)}</span>
          </text>
          <For each={rows()}>
            {(t) => (
              <text fg={STATE_COLOR[t.state] ?? MUTED}>
                {glyph(t.state)} <span fg={agentColor(t.to)}>{t.to}</span> {t.short}
                {t.hop_capped ? " ⚠ hop cap" : ""}
              </text>
            )}
          </For>
        </box>
      </Show>
    );
  };

  const Banner = () => {
    const w = () => waiting(snap());
    const c = () => capped(snap());
    return (
      <Show when={w().length + c().length > 0}>
        <text fg={WARN}>
          ⚠ a2a:{w().length ? ` ${w().length} task(s) need you (/a2a-answer)` : ""}
          {c().length ? ` ${c().length} context(s) hit the hop cap (/a2a-continue)` : ""}
        </text>
      </Show>
    );
  };

  const Footer = () => <text fg={MUTED}>{statusLine(snap())}</text>;

  // keymap.layer belongs to the component that calls it, so it lives in one mounted at the app root
  const Commands = () => {
    context.keymap.layer(() => ({
      commands: [
        { id: "a2a.answer", title: "A2A: answer a task", group: "A2A", palette: true, slash: { name: "a2a-answer" }, run: () => void answer() },
        { id: "a2a.continue", title: "A2A: continue a capped context", group: "A2A", palette: true, slash: { name: "a2a-continue" }, run: () => void cont() },
        { id: "a2a.cancel", title: "A2A: cancel a task", group: "A2A", palette: true, slash: { name: "a2a-cancel" }, run: () => void cancel() },
      ],
    }));
    return <box height={0} />;
  };

  const stops = [
    context.ui.slot({ append: "app", render: () => <Commands /> }),
    context.ui.slot({ append: "sidebar.content", render: () => <Sidebar /> }),
    context.ui.slot({ append: "session.composer.top", render: () => <Banner /> }),
    context.ui.slot({ append: "prompt.footer.status", render: () => <Footer /> }),
    context.ui.slot({ append: "home.footer.status", render: () => <Footer /> }),
  ];

  return () => {
    clearInterval(timer);
    for (const stop of stops) stop();
  };
}
