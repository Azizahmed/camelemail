import { describe, expect, it } from "vitest";
import { aiProblems, isDomain, isGoogleClientId, isGuid, isHost, isHttpUrl, isTenant, presetProblems, providerProblems } from "./admin";
import type { OrgAi, OrgProviders, ServerPreset } from "./types";

const preset = (p: Partial<ServerPreset> = {}): ServerPreset => ({
  name: "Acme",
  domain: "acme.com",
  imapHost: "imap.acme.com",
  imapPort: 993,
  imapSecurity: "tls",
  smtpHost: "smtp.acme.com",
  smtpPort: 465,
  smtpSecurity: "tls",
  caldavUrl: "",
  ...p,
});

const providers = (p: Partial<OrgProviders> = {}): OrgProviders => ({
  google: true,
  microsoft: true,
  imap: true,
  manualServers: true,
  googleApp: { clientId: "", tenant: "" },
  microsoftApp: { clientId: "", tenant: "common" },
  presets: [],
  ...p,
});

const ai = (a: Partial<OrgAi> = {}): OrgAi => ({ enabled: true, provider: "openai", baseUrl: "https://api.openai.com/v1", model: "m", maxTokens: 4096, ...a });

describe("admin checks", () => {
  it("knows IDs, hosts and URLs", () => {
    expect(isGuid("9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6e")).toBe(true);
    expect(isGuid("9832a9e1-5779-4e2f-b2cb-8d2bdf4e9b6")).toBe(false);
    expect(isHost("mail.acme.com") && isHost("localhost") && isHost("10.0.0.5")).toBe(true);
    expect(isHost("mail acme.com") || isHost("-x.com") || isHost("")).toBe(false);
    expect(isDomain("acme.com")).toBe(true);
    expect(isDomain("localhost") || isDomain("10.0.0.5")).toBe(false);
    expect(isHttpUrl("https://dav.acme.com/") && !isHttpUrl("dav.acme.com") && !isHttpUrl("ftp://x.com")).toBe(true);
    expect(isGoogleClientId("123-abc.apps.googleusercontent.com")).toBe(true);
    expect(isGoogleClientId("123-abc")).toBe(false);
    expect(isTenant("organizations") && isTenant("contoso.onmicrosoft.com") && !isTenant("everyone")).toBe(true);
  });

  it("checks servers field by field", () => {
    expect(presetProblems(preset())).toEqual({});
    expect(presetProblems(preset({ name: " ", imapPort: 0, smtpHost: "smtp acme", caldavUrl: "dav.acme.com", domain: "acme" }))).toEqual({
      name: "admin.prov.badName",
      imapPort: "admin.prov.badPort",
      smtpHost: "admin.prov.badHost",
      caldavUrl: "admin.prov.badCaldav",
      domain: "admin.prov.badDomain",
    });
  });

  it("checks providers", () => {
    expect(providerProblems(providers(), null)).toEqual({});
    const p = providers({
      googleApp: { clientId: "nope", tenant: "" },
      microsoftApp: { clientId: "123", tenant: "everyone" },
      presets: [preset(), preset({ imapHost: "" })],
    });
    expect(providerProblems(p, "two words")).toEqual({
      googleId: "admin.prov.badGoogleId",
      googleSecret: "admin.prov.badSecret",
      msId: "admin.prov.badMsId",
      tenant: "admin.prov.badTenant",
      preset1: "admin.prov.badHost",
    });
  });

  it("checks the AI setup", () => {
    expect(aiProblems(ai(), null)).toEqual({});
    expect(aiProblems(ai({ enabled: false, baseUrl: "", model: "" }), null)).toEqual({});
    expect(aiProblems(ai({ baseUrl: "", model: "", maxTokens: 10 }), "a b")).toEqual({
      baseUrl: "admin.ai.needUrl",
      model: "admin.ai.needModel",
      maxTokens: "admin.ai.badTokens",
      key: "admin.ai.badKey",
    });
    expect(aiProblems(ai({ baseUrl: "api.openai.com" }), null).baseUrl).toBe("admin.ai.badUrl");
  });
});
