import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CloudEvent, CloudPhase, CloudStatus } from "./types";

// The window follows `cloud-event`. When this device is signed out, Rust sends
// the new status first and then `signedOut` with the reason: the page reload
// must carry that reason, so the sign-in screens can explain a revoked device.

let emit: (e: CloudEvent) => void = () => {};
const status = (phase: CloudPhase): CloudStatus => ({
  server: "http://localhost:8080",
  defaultServer: "http://localhost:8080",
  signedIn: phase !== "signedOut",
  me: null,
  phase,
  offline: false,
  progress: null,
});

vi.mock("./api", () => ({
  api: { cloudStatus: () => Promise.resolve(status("ready")) },
  onCloudEvent: (cb: (e: CloudEvent) => void) => {
    emit = cb;
    return Promise.resolve(() => {});
  },
}));
vi.mock("./ui.svelte", () => ({ toast: vi.fn() }));

const session = new Map<string, string>();
const reload = vi.fn();

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  session.clear();
  reload.mockClear();
  vi.stubGlobal("sessionStorage", {
    getItem: (k: string) => session.get(k) ?? null,
    setItem: (k: string, v: string) => void session.set(k, v),
    removeItem: (k: string) => void session.delete(k),
  });
  vi.stubGlobal("location", { reload });
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function started() {
  const { initCloud, cloud } = await import("./cloud.svelte");
  await initCloud();
  expect(cloud.status?.phase).toBe("ready");
}

describe("signed out while the app is open", () => {
  it("keeps the reason that follows the status change", async () => {
    await started();
    emit({ kind: "status", status: status("signedOut") });
    emit({ kind: "signedOut", reason: "revoked" });
    await vi.runAllTimersAsync();
    expect(reload).toHaveBeenCalledTimes(1);
    expect(session.get("remail.signedOut")).toBe("revoked");
  });

  it("reloads as a plain sign-out when no reason comes", async () => {
    await started();
    emit({ kind: "status", status: status("signedOut") });
    expect(reload).not.toHaveBeenCalled();
    await vi.runAllTimersAsync();
    expect(reload).toHaveBeenCalledTimes(1);
    expect(session.get("remail.signedOut")).toBe("user");
  });

  it("acts on a reason that comes first", async () => {
    await started();
    emit({ kind: "signedOut", reason: "expired" });
    emit({ kind: "status", status: status("signedOut") });
    await vi.runAllTimersAsync();
    expect(reload).toHaveBeenCalledTimes(1);
    expect(session.get("remail.signedOut")).toBe("expired");
  });
});
