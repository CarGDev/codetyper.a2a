import { expect, test } from "bun:test";
import { binPath, runtimeFile, type Env } from "../plugins/a2a/runtime-home";

const HOME = "/home/u";
const CFG = "/home/u/.config/opencode";
const env = (e: Env = {}, files: string[] = []) => ({
  env: e, home: HOME, config: CFG, exists: (p: string) => files.includes(p),
});

test("A2A_TABS wins over everything", () => {
  expect(runtimeFile("tabs.jsonl", env({ A2A_TABS: "/x/t.jsonl", A2A_DB: "/y/hub.db" }))).toBe("/x/t.jsonl");
});

test("tabs.jsonl follows A2A_DB, then CODETYPERA2A_HOME, then ~/.codetypera2a", () => {
  expect(runtimeFile("tabs.jsonl", env({ A2A_DB: "/y/hub.db" }))).toBe("/y/tabs.jsonl");
  expect(runtimeFile("tabs.jsonl", env({ CODETYPERA2A_HOME: "/h" }))).toBe("/h/tabs.jsonl");
  expect(runtimeFile("tabs.jsonl", env())).toBe("/home/u/.codetypera2a/tabs.jsonl");
});

test("the legacy config folder is used only when the new file is absent and the legacy one present", () => {
  const legacy = `${CFG}/a2a/tabs.jsonl`;
  expect(runtimeFile("tabs.jsonl", env({}, [legacy]))).toBe(legacy);
  expect(runtimeFile("tabs.jsonl", env({}, [legacy, "/home/u/.codetypera2a/tabs.jsonl"]))).toBe("/home/u/.codetypera2a/tabs.jsonl");
  expect(runtimeFile("tabs.jsonl", env({}, []))).toBe("/home/u/.codetypera2a/tabs.jsonl");
});

test("codetypera2a.path uses the same order", () => {
  expect(runtimeFile("codetypera2a.path", env({ CODETYPERA2A_HOME: "/h" }))).toBe("/h/codetypera2a.path");
  expect(runtimeFile("codetypera2a.path", env({}, [`${CFG}/a2a/codetypera2a.path`]))).toBe(`${CFG}/a2a/codetypera2a.path`);
});

test("binPath reads the recorded binary and falls back to PATH when empty or unreadable", () => {
  expect(binPath(env(), () => "  /opt/codetypera2a \n")).toBe("/opt/codetypera2a");
  expect(binPath(env(), () => "  ")).toBe("codetypera2a");
  expect(binPath(env(), () => { throw new Error("nope"); })).toBe("codetypera2a");
});
