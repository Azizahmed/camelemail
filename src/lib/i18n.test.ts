import { describe, expect, it } from "vitest";
import { ar } from "./i18n/ar";
import { en } from "./i18n/en";
import { ERROR_CODES, isCloudError, parseCloudError } from "./cloud-error";
import { errMessage, format, i18n, setLocale, systemLocale, t } from "./i18n.svelte";

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
// Bidi controls belong in code (isolating values), not in the catalogs.
const BIDI_CONTROLS = /[‎‏‪-‮⁦-⁩؜]/;

describe("catalogs", () => {
  it("Arabic has every English key and no others", () => {
    expect(Object.keys(ar).sort()).toEqual(Object.keys(en).sort());
  });

  it("has no empty strings", () => {
    for (const [k, v] of [...Object.entries(en), ...Object.entries(ar)]) expect(v.trim(), k).not.toBe("");
  });

  it("uses the same placeholders in both languages", () => {
    for (const k of Object.keys(en) as (keyof typeof en)[]) expect(placeholders(ar[k]), k).toEqual(placeholders(en[k]));
  });

  it("keeps bidi control characters out of the text", () => {
    for (const [k, v] of [...Object.entries(en), ...Object.entries(ar)]) expect(BIDI_CONTROLS.test(v), k).toBe(false);
  });

  it("is actually translated", () => {
    // Placeholders like an e-mail address may stay the same; real sentences may not.
    const same = (Object.keys(en) as (keyof typeof en)[]).filter((k) => en[k] === ar[k]);
    expect(same).toEqual(["common.emailPlaceholder"]);
  });

  it("has a message for every server error code", () => {
    for (const code of ERROR_CODES) expect(Object.keys(en), code).toContain(`err.${code}`);
    const errKeys = Object.keys(en).filter((k) => k.startsWith("err."));
    expect(errKeys.length).toBe(ERROR_CODES.length);
  });
});

describe("t", () => {
  it("fills placeholders and follows the language", () => {
    setLocale("en", false);
    expect(t("welcome.org", { org: "Acme" })).toBe("Sign in to Acme");
    setLocale("ar", false);
    expect(i18n.locale).toBe("ar");
    // Values are isolated inside right-to-left text.
    expect(t("welcome.org", { org: "Acme" })).toBe("تسجيل الدخول إلى ⁨Acme⁩");
    setLocale("en", false);
  });

  it("leaves unknown placeholders alone", () => {
    expect(format("{a} and {b}", { a: 1 })).toBe("1 and {b}");
    expect(format("{a}", undefined)).toBe("{a}");
  });

  it("picks the system language", () => {
    expect(systemLocale(["ar-SA", "en-US"])).toBe("ar");
    expect(systemLocale(["fr-FR", "ar"])).toBe("ar");
    expect(systemLocale(["fr-FR"])).toBe("en");
    expect(systemLocale([])).toBe("en");
  });
});

describe("server errors", () => {
  it("parses cloud:<code>:<message>", () => {
    expect(parseCloudError("cloud:invalid_credentials:wrong e-mail or password")).toEqual({
      code: "invalid_credentials",
      message: "wrong e-mail or password",
    });
    expect(parseCloudError("cloud:unavailable:connect: refused: 127.0.0.1:8080")).toEqual({
      code: "unavailable",
      message: "connect: refused: 127.0.0.1:8080",
    });
    expect(parseCloudError(new Error("cloud:weak_password:"))).toEqual({ code: "weak_password", message: "" });
    expect(parseCloudError("cloud:rate_limited")).toEqual({ code: "rate_limited", message: "" });
    expect(parseCloudError("IMAP login failed")).toBeNull();
    expect(parseCloudError(undefined)).toBeNull();
    expect(isCloudError("cloud:email_not_verified:x", "email_not_verified")).toBe(true);
    expect(isCloudError("cloud:email_taken:x", "email_not_verified")).toBe(false);
  });

  it("shows known codes translated, others as they came", () => {
    setLocale("en", false);
    expect(errMessage("cloud:invalid_credentials:nope")).toBe(en["err.invalid_credentials"]);
    setLocale("ar", false);
    expect(errMessage("cloud:invalid_credentials:nope")).toBe(ar["err.invalid_credentials"]);
    setLocale("en", false);
    expect(errMessage("cloud:brand_new_code:Something new")).toBe("Something new");
    expect(errMessage("cloud:brand_new_code:")).toBe(en["err.internal"]);
    expect(errMessage(new Error("IMAP login failed"))).toBe("IMAP login failed");
    expect(errMessage("plain")).toBe("plain");
  });
});
