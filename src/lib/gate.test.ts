import { describe, expect, it } from "vitest";
import { extractToken, gateScreen, olderThan, windowReady } from "./gate";
import type { CloudPhase, CloudStatus } from "./types";

const status = (phase: CloudPhase): CloudStatus => ({
  server: "https://mail.example.com",
  defaultServer: null,
  signedIn: phase !== "signedOut",
  me: null,
  phase,
  offline: false,
  progress: null,
});

describe("gate", () => {
  it("waits for the status, or shows why it can't", () => {
    expect(gateScreen({ status: null, accounts: null })).toBe("loading");
    expect(gateScreen({ status: null, error: "no backend", accounts: null })).toBe("error");
  });

  it("maps each phase to its screen", () => {
    expect(gateScreen({ status: status("signedOut"), accounts: null })).toBe("auth");
    expect(gateScreen({ status: status("migrate"), accounts: null })).toBe("migrate");
    expect(gateScreen({ status: status("syncing"), accounts: null })).toBe("syncing");
    expect(gateScreen({ status: status("ready"), accounts: null })).toBe("loading");
    expect(gateScreen({ status: status("ready"), accounts: 0 })).toBe("firstMailbox");
    expect(gateScreen({ status: status("ready"), accounts: 2 })).toBe("app");
  });

  it("lets the first mailbox wait for a calendar-only start", () => {
    expect(gateScreen({ status: status("ready"), accounts: 0, skipFirstMailbox: true })).toBe("app");
  });

  it("keeps the sign-in screens while they still show backup codes", () => {
    expect(gateScreen({ status: status("ready"), accounts: 1, holdAuth: true })).toBe("auth");
    expect(gateScreen({ status: status("syncing"), accounts: null, holdAuth: true })).toBe("auth");
  });

  it("keeps the app once shown, until signed out", () => {
    expect(gateScreen({ status: status("syncing"), accounts: 0, entered: true })).toBe("app");
    expect(gateScreen({ status: status("ready"), accounts: 0, entered: true })).toBe("app");
    expect(gateScreen({ status: status("signedOut"), accounts: 3, entered: true })).toBe("auth");
  });

  it("opens compose and settings windows only when ready", () => {
    expect(windowReady("ready")).toBe(true);
    expect(windowReady("syncing")).toBe(false);
    expect(windowReady(undefined)).toBe(false);
  });
});

describe("pasted links", () => {
  it("takes the token out of a link, or the code as it is", () => {
    expect(extractToken("remail://verify?t=abc123")).toBe("abc123");
    expect(extractToken("  https://mail.example.com/verify?lang=en&token=a%2Bb  ")).toBe("a+b");
    expect(extractToken("https://x/reset#code=XYZ")).toBe("XYZ");
    expect(extractToken(" abcd efgh ")).toBe("abcdefgh");
    expect(extractToken("")).toBe("");
  });
});

describe("versions", () => {
  it("compares numerically", () => {
    expect(olderThan("0.2.1", "0.10.0")).toBe(true);
    expect(olderThan("1.0.0", "1.0.0")).toBe(false);
    expect(olderThan("1.2", "1.1.9")).toBe(false);
    expect(olderThan("v1.0.0-beta.1", "1.0.1")).toBe(true);
  });
});
