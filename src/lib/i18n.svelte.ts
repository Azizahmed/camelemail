// Translations. `t(key, vars)` looks the key up in the current language's
// catalog (src/lib/i18n/*.ts). The language comes from the synced setting
// `language` once settings are loaded; before that (and before signing in)
// from a localStorage mirror, else the system language. Changing it sets
// <html lang dir>, so right-to-left layouts follow.

import { ar } from "./i18n/ar";
import { en, type MessageKey } from "./i18n/en";
import { isErrorCode, parseCloudError } from "./cloud-error";

export type { MessageKey } from "./i18n/en";
export type Locale = "en" | "ar";
export type LanguageSetting = Locale | "system";

/** Languages offered, each named in itself. */
export const LOCALES: { value: Locale; label: string }[] = [
  { value: "en", label: "English" },
  { value: "ar", label: "العربية" },
];

const CATALOGS: Record<Locale, Record<MessageKey, string>> = { en, ar };

/** Mirror of the language, readable before sign-in and shared by every window. */
export const LANGUAGE_KEY = "language";

const isLocale = (v: unknown): v is Locale => v === "en" || v === "ar";

/** The system's preferred language among ours (Arabic for any `ar-*`). */
export function systemLocale(languages: readonly string[] = typeof navigator === "undefined" ? [] : navigator.languages ?? [navigator.language]): Locale {
  for (const l of languages) {
    const base = l?.toLowerCase().split(/[-_]/)[0];
    if (isLocale(base)) return base;
  }
  return "en";
}

function stored(): Locale | null {
  try {
    const v = typeof localStorage === "undefined" ? null : localStorage.getItem(LANGUAGE_KEY);
    return isLocale(v) ? v : null;
  } catch {
    return null;
  }
}

export const i18n = $state<{ locale: Locale }>({ locale: stored() ?? systemLocale() });

export const dirOf = (l: Locale): "ltr" | "rtl" => (l === "ar" ? "rtl" : "ltr");

/** BCP 47 tag for Intl formatting (Arabic keeps Western digits, like the rest of the app). */
export const intlLocale = () => (i18n.locale === "ar" ? "ar-u-nu-latn" : "en");

function applyDocument() {
  if (typeof document === "undefined") return;
  document.documentElement.lang = i18n.locale;
  document.documentElement.dir = dirOf(i18n.locale);
}
applyDocument();

/** Switch language now. `remember` keeps it for the sign-in screens and other windows. */
export function setLocale(locale: Locale, remember = true) {
  if (remember) {
    try {
      localStorage.setItem(LANGUAGE_KEY, locale);
    } catch {
      /* private mode: this window only */
    }
  }
  if (i18n.locale === locale) return;
  i18n.locale = locale;
  applyDocument();
}

/**
 * Follow the synced `language` setting. "system" (or none) keeps a language
 * picked on the sign-in screens, else follows the system.
 */
export function applyLanguageSetting(setting: LanguageSetting | null | undefined) {
  if (isLocale(setting)) setLocale(setting);
  else setLocale(stored() ?? systemLocale(), false);
}

if (typeof window !== "undefined") {
  // Another window changed it.
  window.addEventListener("storage", (e) => {
    if (e.key === LANGUAGE_KEY && isLocale(e.newValue)) setLocale(e.newValue, false);
  });
}

// U+2068 FIRST STRONG ISOLATE … U+2069 POP DIRECTIONAL ISOLATE: an e-mail
// address or a name keeps its own direction inside a sentence.
const FSI = "⁨";
const PDI = "⁩";

/** Fill {placeholders}. In right-to-left text each value is isolated. */
export function format(template: string, vars?: Record<string, string | number>, rtl = false): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) => {
    if (!(name in vars)) return whole;
    const v = String(vars[name]);
    return rtl ? FSI + v + PDI : v;
  });
}

/** The current language's text for `key`, with `vars` filled in. */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const l = i18n.locale;
  const s = CATALOGS[l][key] ?? en[key] ?? key;
  return format(s, vars, l === "ar");
}

export const hasMessage = (key: string): key is MessageKey => key in en;

/**
 * What to show for a rejected invoke: a server error ("cloud:<code>:…") in
 * the current language, anything else as it came.
 */
export function errMessage(e: unknown): string {
  const c = parseCloudError(e);
  if (c) {
    const key = `err.${c.code}`;
    if (isErrorCode(c.code) && hasMessage(key)) return t(key);
    return c.message || t("err.internal");
  }
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
