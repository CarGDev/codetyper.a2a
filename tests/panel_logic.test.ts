import { expect, test } from "bun:test";
import { bar, barParts, progressLabel, changes, cost, EMPTY, frontmatterColor, hashColor, statusLine, tokens, type Snapshot, type Task } from "../plugins/a2a/panel-logic";

const task = (o: Partial<Task>): Task => ({
  id: "t1", short: "t1", context: "c", from: "orchestrator", to: "mentor", state: "working",
  updated_at: "", directory: null, question: null, hop_capped: false, ...o,
});
const snap = (...tasks: Task[]): Snapshot => ({ ...EMPTY, tasks });

test("progress bar", () => {
  expect(bar(6, 12)).toBe("██████──────");
  expect(bar(0, 0)).toBe("────────────");
  expect(bar(9, 5)).toBe("████████████");
  expect(barParts(3, 12)).toEqual(["███", "─────────"]);
  expect(progressLabel(6, 12)).toBe("6.0 / 12");
});

test("number formatting", () => {
  expect(tokens(1_600_000)).toBe("1.6M");
  expect(tokens(950)).toBe("950");
  expect(cost(0)).toBe("~$0.0000");
  expect(cost(12.345)).toBe("~$12.35");
});

test("the first snapshot only seeds, later ones report transitions", () => {
  const seen = new Map<string, string>();
  expect(changes(seen, snap(task({ state: "working" })))).toEqual([]);
  expect(changes(seen, snap(task({ state: "working" })))).toEqual([]);
  const q = changes(seen, snap(task({ state: "input-required" })));
  expect(q.map((c) => c.kind)).toEqual(["input"]);
  expect(changes(seen, snap(task({ state: "completed" }))).map((c) => c.kind)).toEqual(["done"]);
  expect(changes(seen, snap(task({ id: "t2", state: "failed" }))).map((c) => c.kind)).toEqual(["failed"]);
});

test("a hop cap is reported once", () => {
  const seen = new Map<string, string>();
  changes(seen, snap(task({})));
  expect(changes(seen, snap(task({ hop_capped: true }))).map((c) => c.kind)).toEqual(["cap"]);
  expect(changes(seen, snap(task({ hop_capped: true })))).toEqual([]);
});

test("agent colours: frontmatter first, hash otherwise", () => {
  expect(frontmatterColor("---\ncolor: \"#112233\"\nmode: x\n---\nbody")).toBe("#112233");
  expect(frontmatterColor("---\ncolor: accent\n---\n")).toBe("#2ac3de");
  expect(frontmatterColor("---\nmode: x\n---\n")).toBeUndefined();
  expect(hashColor("mentor")).toBe(hashColor("mentor"));
  expect(hashColor("mentor")).toMatch(/^#[0-9a-f]{6}$/);
});

test("status line", () => {
  const s: Snapshot = { ...snap(task({}), task({ id: "t2", state: "completed" })), dispatcher: true,
    today: { tokens_in: 1_000_000, tokens_out: 500_000, cost_usd: 0 } };
  expect(statusLine(s)).toBe("a2a ● 1 open · ~$0.0000 · 1.5M tok today");
});
