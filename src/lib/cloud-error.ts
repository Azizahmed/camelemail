// Errors from the Camel Mail server reach the UI as rejected invokes whose text is
// "cloud:<code>:<message>" (docs/platform/client-contract.md). The code is a
// proto `ErrorCode` wire name; the UI shows t("err.<code>"), not the message.

/** Every proto `ErrorCode` (crates/remail-proto/src/error.rs), in wire form. */
export const ERROR_CODES = [
  "bad_request",
  "unauthorized",
  "forbidden",
  "not_found",
  "conflict",
  "version_mismatch",
  "too_large",
  "rate_limited",
  "invalid_credentials",
  "account_locked",
  "account_disabled",
  "email_not_verified",
  "email_taken",
  "weak_password",
  "invalid_code",
  "invalid_token",
  "two_factor_required",
  "signup_closed",
  "invite_required",
  "domain_not_allowed",
  "provider_not_allowed",
  "platform_not_allowed",
  "licence_expired",
  "licence_invalid",
  "seat_limit",
  "update_required",
  "unavailable",
  "internal",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface CloudError {
  /** The wire code; may be one this build doesn't know yet. */
  code: string;
  message: string;
}

const KNOWN = new Set<string>(ERROR_CODES);

export const isErrorCode = (code: string): code is ErrorCode => KNOWN.has(code);

function text(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e && typeof e.message === "string") return e.message;
  return "";
}

/** The code and message of a "cloud:<code>:<message>" rejection, or null for any other error. */
export function parseCloudError(e: unknown): CloudError | null {
  const m = /^cloud:([a-z0-9_]+)(?::([\s\S]*))?$/.exec(text(e).trim());
  return m ? { code: m[1], message: (m[2] ?? "").trim() } : null;
}

/** Is `e` a server error with this code? */
export const isCloudError = (e: unknown, code: ErrorCode) => parseCloudError(e)?.code === code;
