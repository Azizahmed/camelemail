// Checks for what admins save (Settings › Admin), mirroring the server's
// (crates/remail-server/src/admin/validate.rs) so mistakes show next to the
// field, translated, before anything is sent.

import type { MessageKey } from "./i18n.svelte";
import type { OrgAi, OrgProviders, ServerPreset } from "./types";

export const isGuid = (s: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(s);

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;

/** A DNS name (`mail.acme.com`, `localhost`) or an IP address. */
export function isHost(s: string): boolean {
  if (IPV4.test(s) || (s.includes(":") && /^[0-9a-f:.]+$/i.test(s))) return true;
  return (
    s.length > 0 &&
    s.length <= 253 &&
    s.split(".").every((l) => l.length > 0 && l.length <= 63 && /^[a-z0-9-]+$/i.test(l) && !l.startsWith("-") && !l.endsWith("-"))
  );
}

/** A DNS name with at least one dot. */
export const isDomain = (s: string) => s.includes(".") && !IPV4.test(s) && isHost(s);

export function isHttpUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return (u.protocol === "https:" || u.protocol === "http:") && u.hostname.length > 0;
  } catch {
    return false;
  }
}

export const isGoogleClientId = (s: string) => s.length <= 200 && /^[A-Za-z0-9_-]+\.apps\.googleusercontent\.com$/.test(s);

export const isTenant = (s: string) => ["common", "organizations", "consumers"].includes(s) || isGuid(s) || isDomain(s);

/** A pasted key or secret: one token, no spaces. */
export const isSecret = (s: string) => s.trim().length <= 2000 && !/\s/.test(s.trim());

/** Field → what's wrong with it. Empty: fine. */
export type Problems = Record<string, MessageKey>;

export function presetProblems(p: ServerPreset): Problems {
  const out: Problems = {};
  if (!p.name.trim() || p.name.trim().length > 100) out.name = "admin.prov.badName";
  const domain = p.domain.trim().replace(/^@/, "");
  if (domain && !isDomain(domain)) out.domain = "admin.prov.badDomain";
  if (!isHost(p.imapHost.trim())) out.imapHost = "admin.prov.badHost";
  if (!isHost(p.smtpHost.trim())) out.smtpHost = "admin.prov.badHost";
  const port = (n: number) => Number.isInteger(n) && n >= 1 && n <= 65535;
  if (!port(p.imapPort)) out.imapPort = "admin.prov.badPort";
  if (!port(p.smtpPort)) out.smtpPort = "admin.prov.badPort";
  if (p.caldavUrl.trim() && !isHttpUrl(p.caldavUrl.trim())) out.caldavUrl = "admin.prov.badCaldav";
  return out;
}

/** `googleSecret`: null keeps the saved one. Preset problems are keyed `preset<i>`. */
export function providerProblems(p: OrgProviders, googleSecret: string | null): Problems {
  const out: Problems = {};
  const gid = p.googleApp.clientId.trim();
  if (gid && !isGoogleClientId(gid)) out.googleId = "admin.prov.badGoogleId";
  if (googleSecret !== null && googleSecret.trim() && !isSecret(googleSecret)) out.googleSecret = "admin.prov.badSecret";
  const mid = p.microsoftApp.clientId.trim();
  if (mid && !isGuid(mid)) out.msId = "admin.prov.badMsId";
  const tenant = p.microsoftApp.tenant.trim().toLowerCase();
  if (tenant && !isTenant(tenant)) out.tenant = "admin.prov.badTenant";
  p.presets.forEach((x, i) => {
    const first = Object.values(presetProblems(x))[0];
    if (first) out[`preset${i}`] = first;
  });
  return out;
}

/** `key`: null keeps the saved one. */
export function aiProblems(ai: OrgAi, key: string | null): Problems {
  const out: Problems = {};
  const url = ai.baseUrl.trim();
  if (url && !isHttpUrl(url)) out.baseUrl = "admin.ai.badUrl";
  else if (ai.enabled && !url) out.baseUrl = "admin.ai.needUrl";
  if (ai.enabled && !ai.model.trim()) out.model = "admin.ai.needModel";
  if (!Number.isInteger(ai.maxTokens) || ai.maxTokens < 256 || ai.maxTokens > 1_000_000) out.maxTokens = "admin.ai.badTokens";
  if (key !== null && key.trim() && !isSecret(key)) out.key = "admin.ai.badKey";
  return out;
}

/** A local server (Ollama, LM Studio, …) works without a key. */
export const isLocalUrl = (s: string) => /:\/\/(localhost|127\.|\[::1\]|0\.0\.0\.0)/i.test(s);

/** The three parts of Settings › Admin (`openSettings("admin:<part>")`). */
export type AdminTab = "ai" | "providers" | "admins";
export const ADMIN_TABS: readonly AdminTab[] = ["ai", "providers", "admins"];
