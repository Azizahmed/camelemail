// Which screen the main window shows (docs/platform/client-contract.md, "Gate").

import type { CloudPhase, CloudStatus } from "./types";

export type GateScreen = "loading" | "error" | "auth" | "migrate" | "syncing" | "firstMailbox" | "app";

export interface GateInput {
  status: CloudStatus | null;
  /** `cloud_status` failed and nothing is known yet. */
  error?: string | null;
  /** Mailboxes on this device, once known (only asked for when ready). */
  accounts: number | null;
  /** The auth screens still have something to show (backup codes after a required 2FA setup). */
  holdAuth?: boolean;
  /** The app was already shown in this session: it stays (only signing out leaves it). */
  entered?: boolean;
  /** "Only need a calendar?" was chosen on the first-mailbox screen. */
  skipFirstMailbox?: boolean;
}

export function gateScreen({ status, error, accounts, holdAuth, entered, skipFirstMailbox }: GateInput): GateScreen {
  if (!status) return error ? "error" : "loading";
  if (status.phase === "signedOut" || holdAuth) return "auth";
  if (entered) return "app";
  if (status.phase === "migrate") return "migrate";
  if (status.phase === "syncing") return "syncing";
  if (accounts === null) return "loading";
  if (accounts === 0 && !skipFirstMailbox) return "firstMailbox";
  return "app";
}

/** Compose and settings windows wait for this. */
export const windowReady = (phase: CloudPhase | undefined) => phase === "ready";

/**
 * The token in what was pasted from a verification or reset e-mail: the whole
 * link (remail://verify?t=…, https://…/verify?token=…) or just the code.
 */
/** Invitation codes (`inv_…`) go to sign-up, not to the e-mail confirmation. */
export function isInviteToken(token: string): boolean {
  return token.startsWith("inv_");
}

export function extractToken(input: string): string {
  const s = input.trim();
  if (!s) return "";
  const m = /[?&#](?:t|token|code)=([^&#\s]+)/.exec(s);
  if (m) {
    try {
      return decodeURIComponent(m[1]);
    } catch {
      return m[1];
    }
  }
  return s.replace(/\s+/g, "");
}

/** Is version `a` older than `b` ("0.2.1" < "0.10.0"; pre-release tags ignored)? */
export function olderThan(a: string, b: string): boolean {
  const parse = (v: string) => v.replace(/^v/, "").split(/[-+]/)[0].split(".").map((n) => Number.parseInt(n, 10) || 0);
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d < 0;
  }
  return false;
}
