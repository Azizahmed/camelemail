<!--
  Everything shown while signed out: welcome, server or organisation code,
  create account, confirm e-mail, sign in, two-step verification (and its
  required setup), password reset. Signing in changes the cloud status, and
  the gate in App.svelte moves on from there.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../../lib/api";
  import { cloud, refreshCloud } from "../../lib/cloud.svelte";
  import { isCloudError, parseCloudError } from "../../lib/cloud-error";
  import { olderThan } from "../../lib/gate";
  import { t } from "../../lib/i18n.svelte";
  import type { Meta } from "../../lib/types";
  import { toast } from "../../lib/ui.svelte";
  import AuthScreen from "./AuthScreen.svelte";
  import ResetPassword from "./ResetPassword.svelte";
  import ResetRequest from "./ResetRequest.svelte";
  import ServerChoice from "./ServerChoice.svelte";
  import SignIn from "./SignIn.svelte";
  import SignUp from "./SignUp.svelte";
  import TwoFactor from "./TwoFactor.svelte";
  import TwoFactorSetup from "./TwoFactorSetup.svelte";
  import VerifyEmail from "./VerifyEmail.svelte";
  import Welcome from "./Welcome.svelte";

  type Screen = "welcome" | "server" | "signup" | "verify" | "signin" | "twofa" | "totp" | "reset" | "resetPassword";

  let screen = $state<Screen>("welcome");
  /** Where to go once a server is chosen (Create / Sign in without one). */
  let afterServer: Screen = "welcome";
  let meta = $state<Meta | null>(null);
  let connecting = $state(false);
  let unreachable = $state(false);
  let appVersion = $state<string | null>(null);
  let email = $state("");
  /** An invitation code pasted on the confirmation screen, for sign-up. */
  let inviteCode = $state("");
  let notice = $state("");
  let challenge = $state("");
  /** Kept in memory only, to sign in right after the e-mail is confirmed. */
  let pending: { email: string; password: string } | null = null;

  const status = $derived(cloud.status);
  const server = $derived(status?.server ?? status?.defaultServer ?? null);
  const custom = $derived(!!status?.server && status.server !== status.defaultServer);
  const outdated = $derived(meta && appVersion && olderThan(appVersion, meta.minClient) ? meta.minClient : null);

  async function connect(address: string) {
    connecting = true;
    unreachable = false;
    try {
      meta = await api.cloudSetServer(address);
      await refreshCloud();
    } catch (e) {
      // An unknown code or a bad address is for the server screen to explain.
      unreachable = parseCloudError(e)?.code === "unavailable" || !parseCloudError(e);
      if (!unreachable) screen = "server";
    } finally {
      connecting = false;
    }
  }

  onMount(() => {
    api.appInfo().then((i) => (appVersion = i.version)).catch(() => {});
    if (server) void connect(server);
  });

  function go(next: Screen) {
    notice = next === "signin" ? notice : "";
    screen = next;
  }

  /** Create / Sign in: a server first, if none is known. */
  function start(next: "signup" | "signin") {
    if (!server) {
      afterServer = next;
      go("server");
    } else go(next);
  }

  function serverChosen(m: Meta) {
    meta = m;
    unreachable = false;
    if (m.orgName) toast(t("server.connected", { name: m.orgName }));
    void refreshCloud();
    go(afterServer);
    afterServer = "welcome";
  }

  /** Sign in; a 2FA challenge moves on to the code (or its required setup). */
  async function login(address: string, password: string) {
    email = address;
    try {
      const r = await api.cloudLogin(address, password);
      pending = null;
      if (r.status === "ok") {
        await refreshCloud();
        return;
      }
      challenge = r.challenge;
      go(r.setupRequired ? "totp" : "twofa");
    } catch (e) {
      if (isCloudError(e, "email_not_verified")) {
        pending = { email: address, password };
        go("verify");
        return;
      }
      throw e;
    }
  }

  async function verify2fa(code: string) {
    await api.cloudLogin2fa(challenge, code);
    await refreshCloud();
  }

  function expired() {
    challenge = "";
    notice = t("twofa.expired");
    go("signin");
  }

  async function created(verify: "email" | "none", password: string) {
    pending = { email, password };
    if (verify === "email") {
      go("verify");
      return;
    }
    try {
      await login(email, password);
    } catch {
      go("signin");
    }
  }

  async function verified() {
    const p = pending;
    if (p) {
      try {
        await login(p.email, p.password);
        return;
      } catch {
        /* sign in by hand below */
      }
    }
    notice = t("verify.done");
    go("signin");
  }

  function totpDone() {
    cloud.holdAuth = false;
    challenge = "";
    void refreshCloud();
  }
</script>

{#if screen === "welcome"}
  <Welcome
    {meta}
    {server}
    {custom}
    {connecting}
    {unreachable}
    {outdated}
    oncreate={() => start("signup")}
    onsignin={() => start("signin")}
    onserver={() => go("server")}
    onretry={() => server && connect(server)}
  />
{:else if screen === "server"}
  <ServerChoice
    current={status?.server ?? null}
    defaultServer={status?.defaultServer ?? null}
    onconnected={serverChosen}
    onback={() => go("welcome")}
  />
{:else if screen === "signup"}
  {#key inviteCode}
    <SignUp {meta} bind:email invite={inviteCode} oncreated={created} onsignin={() => go("signin")} onback={() => go("welcome")} />
  {/key}
{:else if screen === "verify"}
  <VerifyEmail
    {email}
    onverified={verified}
    onsignin={() => go("signin")}
    oninvite={(code) => {
      inviteCode = code;
      go("signup");
    }}
  />
{:else if screen === "signin"}
  <SignIn {meta} bind:email {notice} onlogin={login} oncreate={() => go("signup")} onforgot={() => go("reset")} onback={() => go("welcome")} />
{:else if screen === "twofa"}
  <TwoFactor onverify={verify2fa} onexpired={expired} onback={() => go("signin")} />
{:else if screen === "totp"}
  <AuthScreen title={t("totp.title")} onback={cloud.holdAuth ? undefined : () => go("signin")}>
    <TwoFactorSetup {challenge} ondone={totpDone} onexpired={expired} />
  </AuthScreen>
{:else if screen === "reset"}
  <ResetRequest bind:email onhavelink={() => go("resetPassword")} onback={() => go("signin")} />
{:else if screen === "resetPassword"}
  <ResetPassword
    {email}
    ondone={() => {
      notice = t("reset.done");
      go("signin");
    }}
    onback={() => go("reset")}
  />
{/if}
