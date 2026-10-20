// UI-wide helpers shared by every window: toasts, confirm dialogs, theme.

import type { AppSettings } from "./types";

export interface ToastAction {
  label: string;
  run: () => void | Promise<void>;
}

export interface Toast {
  id: number;
  message: string;
  kind: "info" | "success" | "error";
  action?: ToastAction;
}

export const toasts = $state<{ current: Toast | null }>({ current: null });

let toastSeq = 0;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

/** Show a toast (one at a time; a new one replaces the old). */
export function toast(message: string, opts: { kind?: Toast["kind"]; action?: ToastAction; duration?: number } = {}) {
  clearTimeout(toastTimer);
  const t: Toast = { id: ++toastSeq, message, kind: opts.kind ?? "info", action: opts.action };
  toasts.current = t;
  const duration = opts.duration ?? (opts.action ? 6000 : 3000);
  toastTimer = setTimeout(() => {
    if (toasts.current?.id === t.id) toasts.current = null;
  }, duration);
}

export function dismissToast() {
  toasts.current = null;
}

export function toastError(e: unknown, prefix?: string) {
  const msg = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  toast(prefix ? `${prefix}: ${msg}` : msg, { kind: "error", duration: 5000 });
}

// ---------------------------------------------------------------------------
// Confirm dialog (rendered by <ConfirmDialog/>)
// ---------------------------------------------------------------------------

export interface ConfirmRequest {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel: string;
  danger: boolean;
  /** Optional third button (e.g. "Discard"). */
  altLabel?: string;
  resolve: (v: "confirm" | "cancel" | "alt") => void;
}

export const dialogs = $state<{ confirm: ConfirmRequest | null }>({ confirm: null });

export function confirmDialog(opts: {
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  altLabel?: string;
}): Promise<"confirm" | "cancel" | "alt"> {
  return new Promise((resolve) => {
    dialogs.confirm = {
      title: opts.title,
      message: opts.message,
      confirmLabel: opts.confirmLabel ?? "OK",
      cancelLabel: opts.cancelLabel ?? "Cancel",
      danger: opts.danger ?? false,
      altLabel: opts.altLabel,
      resolve: (v) => {
        dialogs.confirm = null;
        resolve(v);
      },
    };
  });
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

const media = window.matchMedia("(prefers-color-scheme: dark)");
let themeSetting: AppSettings["theme"] = "light";

function apply() {
  const dark = themeSetting === "dark" || (themeSetting === "system" && media.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

media.addEventListener("change", apply);

export function applyTheme(theme: AppSettings["theme"]) {
  themeSetting = theme;
  apply();
}

// ---------------------------------------------------------------------------
// Popover positioning
// ---------------------------------------------------------------------------

/** Place a fixed-position menu next to an anchor rect, keeping it on screen. */
export function placeMenu(anchor: DOMRect, width: number, height: number, align: "left" | "right" = "right") {
  const margin = 8;
  let left = align === "right" ? anchor.right - width : anchor.left;
  left = Math.max(margin, Math.min(left, window.innerWidth - width - margin));
  let top = anchor.bottom + 6;
  if (top + height > window.innerHeight - margin) top = Math.max(margin, anchor.top - height - 6);
  return { left, top };
}

/** Is focus in a text field (where single-key shortcuts must not fire)? */
export function isTyping(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  if (!t) return false;
  const tag = t.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t.isContentEditable || !!t.closest("[data-inline-composer]");
}
