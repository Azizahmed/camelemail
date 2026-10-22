// Whether Android lets Camel Mail show notifications (Android 13+ asks the user).
// Shared by the settings home (its "Permission needed" dot) and the
// Notifications page, and checked again whenever the app comes back to the
// front, since the user may have changed it in Android's own settings.

import { invoke } from "@tauri-apps/api/core";

export type Permission = "granted" | "denied" | "prompt";

class NotifyState {
  /** null until known (or when the platform can't tell). */
  permission = $state<Permission | null>(null);
}

export const notify = new NotifyState();

/** Read the permission, or ask for it with `ask` (shows Android's dialog unless already decided). */
export async function checkPermission(ask = false): Promise<Permission | null> {
  try {
    const p = await invoke<string>("notification_permission", { ask });
    notify.permission = p === "granted" || p === "denied" ? p : "prompt";
  } catch {
    /* unknown: leave it as it was */
  }
  return notify.permission;
}

/** Keep the permission current while a settings screen is shown; returns the cleanup. */
export function followPermission(): () => void {
  void checkPermission();
  const onVisible = () => {
    if (document.visibilityState === "visible") void checkPermission();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => document.removeEventListener("visibilitychange", onVisible);
}
