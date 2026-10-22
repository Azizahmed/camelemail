// The Camel Mail account as this window sees it: `cloud_status`, kept current by
// `cloud-event`. App.svelte gates every window on it (see gate.ts).

import { api, onCloudEvent } from "./api";
import { errMessage, t } from "./i18n.svelte";
import type { CloudEvent, CloudStatus, Me } from "./types";
import { toast } from "./ui.svelte";

class CloudState {
  status = $state<CloudStatus | null>(null);
  /** `cloud_status` failed before anything was known. */
  error = $state<string | null>(null);
  /**
   * The sign-in screens still have something to show although the session is
   * up (the backup codes after a required two-step setup): stay on them.
   */
  holdAuth = $state(false);
}

export const cloud = new CloudState();

/** Set before reloading after a sign-out, so the fresh page can explain it. */
const SIGNED_OUT_KEY = "remail.signedOut";

let reloading = false;

/**
 * Signed out (by the user, another device, the admin, or an expired session):
 * this window's state belongs to the old session, so start over from a clean
 * page. The sign-in screens explain why after the reload.
 */
function signedOut(reason: string) {
  if (reloading) return;
  reloading = true;
  try {
    sessionStorage.setItem(SIGNED_OUT_KEY, reason);
  } catch {
    /* the explanation is optional */
  }
  location.reload();
}

/** A session just ended: reload soon, unless the `signedOut` event names the reason first. */
let endedAt: ReturnType<typeof setTimeout> | null = null;

function setStatus(next: CloudStatus) {
  const prev = cloud.status;
  cloud.status = next;
  cloud.error = null;
  // The status change comes just before the `signedOut` event with its reason
  // (revoked, expired): wait for it briefly, so the reload can explain it.
  if (prev && prev.phase !== "signedOut" && next.phase === "signedOut") endedAt ??= setTimeout(() => signedOut("user"), 400);
}

function onEvent(e: CloudEvent) {
  if (e.kind === "status") setStatus(e.status);
  else if (e.kind === "signedOut" && (endedAt !== null || cloud.status?.phase !== "signedOut")) {
    if (endedAt !== null) clearTimeout(endedAt);
    signedOut(e.reason);
  } else if (e.kind === "signedOut" && cloud.status) cloud.status = { ...cloud.status, phase: "signedOut", signedIn: false, me: null };
}

export async function refreshCloud(): Promise<CloudStatus | null> {
  try {
    const s = await api.cloudStatus();
    setStatus(s);
    return s;
  } catch (e) {
    if (!cloud.status) cloud.error = errMessage(e);
    return null;
  }
}

let started: Promise<void> | null = null;

/** Start following the account (once per window). */
export function initCloud(): Promise<void> {
  started ??= (async () => {
    let reason: string | null = null;
    try {
      reason = sessionStorage.getItem(SIGNED_OUT_KEY);
      sessionStorage.removeItem(SIGNED_OUT_KEY);
    } catch {
      /* none */
    }
    if (reason === "revoked" || reason === "expired")
      toast(t(reason === "revoked" ? "gate.signedOut.revoked" : "gate.signedOut.expired"), { kind: "info", duration: 12000 });
    try {
      await onCloudEvent(onEvent);
    } catch (e) {
      console.warn("No cloud events", e);
    }
    await refreshCloud();
  })();
  return started;
}

/** Retry after `cloud_status` failed. */
export function retryCloud() {
  cloud.error = null;
  void refreshCloud();
}

/** The signed-in user changed (name, two-step verification). */
export function setMe(me: Me) {
  if (cloud.status) cloud.status = { ...cloud.status, me };
}

/** Sign out of Camel Mail on this device (the page reloads when the status says so). */
export async function signOut() {
  await api.cloudLogout();
  const s = await refreshCloud();
  if (!s || s.phase === "signedOut") signedOut("user");
}
