import { beforeEach, describe, expect, it, vi } from "vitest";

// Each call resolves when the test says so, so completion order is under test control.
const pending: { cmd: string; resolve: (v: unknown) => void; reject: (e: unknown) => void }[] = [];
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => new Promise((resolve, reject) => pending.push({ cmd, resolve, reject })),
  Channel: class {},
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
const { invokeInOrder } = await import("./api");

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("invokeInOrder", () => {
  beforeEach(() => {
    pending.length = 0;
  });

  it("starts a call only after the previous one in its queue finished", async () => {
    const done: string[] = [];
    void invokeInOrder("star").then(() => done.push("star"));
    void invokeInOrder("unstar", undefined, "star").then(() => done.push("unstar"));
    await flush();
    expect(pending.map((p) => p.cmd)).toEqual(["star"]);
    pending[0].resolve(undefined);
    await flush();
    expect(pending.map((p) => p.cmd)).toEqual(["star", "unstar"]);
    pending[1].resolve(undefined);
    await flush();
    expect(done).toEqual(["star", "unstar"]);
  });

  it("doesn't hold other queues back", async () => {
    void invokeInOrder("save_draft", undefined, "draft:a");
    void invokeInOrder("save_draft", undefined, "draft:b");
    await flush();
    expect(pending).toHaveLength(2);
  });

  it("goes on after a failed call", async () => {
    const first = invokeInOrder("send", undefined, "draft:c");
    const second = invokeInOrder("delete_draft", undefined, "draft:c");
    await flush();
    pending[0].reject(new Error("offline"));
    await expect(first).rejects.toThrow("offline");
    await flush();
    expect(pending.map((p) => p.cmd)).toEqual(["send", "delete_draft"]);
    pending[1].resolve("ok");
    await expect(second).resolves.toBe("ok");
  });
});
