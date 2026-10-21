// A quick local estimate of password strength for the meter on the sign-up,
// reset and change-password forms. The server is authoritative (zxcvbn score
// 3 or more, and at least 10 characters); this only guides the person typing.

export type Score = 0 | 1 | 2 | 3 | 4;
export type PasswordHint = "length" | "common" | "sequence" | "repeat" | "personal" | "variety";

export interface Strength {
  score: Score;
  /** What would make it stronger, most useful first. */
  hints: PasswordHint[];
}

/** The server's minimum length. */
export const MIN_LENGTH = 10;

// The most used passwords and the words they're built from.
const COMMON = new Set(
  (
    "password passw0rd p@ssw0rd p@ssword password1 qwerty qwertyuiop qwerty123 azerty 123456 1234567 12345678 123456789 1234567890 " +
    "111111 000000 123123 654321 abc123 iloveyou admin administrator welcome welcome1 letmein monkey dragon master sunshine princess " +
    "football baseball superman batman trustno1 shadow michael jennifer freedom whatever starwars login secret changeme hello " +
    "hello123 charlie donald qazwsx zaq12wsx 1q2w3e4r 1q2w3e4r5t mustang access flower computer internet samsung google remail " +
    "email mypassword pass1234 test1234 testtest guest summer winter spring autumn"
  ).split(" "),
);
// Words that weaken a password wherever they appear in it.
const COMMON_WORDS = ["password", "passw0rd", "qwerty", "letmein", "welcome", "iloveyou", "admin", "remail", "monkey", "dragon", "sunshine", "princess", "football"];
const KEYBOARD_ROWS = ["qwertyuiop", "asdfghjkl", "zxcvbnm", "1234567890", "azertyuiop", "qwertzuiop"];

const LOWER = /[a-z]/;
const UPPER = /[A-Z]/;
const DIGIT = /[0-9]/;
const SYMBOL = /[\x20-\x2f\x3a-\x40\x5b-\x60\x7b-\x7e]/;
// Any other character (Arabic letters, accented letters, emoji…).
const OTHER = /[^\x00-\x7f]/;

/** Words from the person's name and e-mail address worth keeping out of the password. */
export function personalWords(...values: (string | null | undefined)[]): string[] {
  const out = new Set<string>();
  for (const v of values) {
    if (!v) continue;
    const local = v.includes("@") ? v.slice(0, v.indexOf("@")) : v;
    for (const w of local.toLowerCase().split(/[\s._+\-@]+/)) if (w.length >= 3) out.add(w);
    if (v.includes("@")) {
      const whole = local.toLowerCase();
      if (whole.length >= 3) out.add(whole);
    }
  }
  return [...out];
}

/** Length of the runs of characters that follow on from each other (abcd, 4321, aaaa) and keyboard rows (qwer). */
function patternChars(pw: string): { sequence: number; repeat: number } {
  const lower = pw.toLowerCase();
  let sequence = 0;
  let repeat = 0;
  // Ascending / descending runs and repeats of 3 or more.
  let i = 0;
  while (i < lower.length - 1) {
    const d = lower.charCodeAt(i + 1) - lower.charCodeAt(i);
    if (d !== 0 && Math.abs(d) !== 1) {
      i++;
      continue;
    }
    let j = i + 1;
    while (j < lower.length - 1 && lower.charCodeAt(j + 1) - lower.charCodeAt(j) === d) j++;
    const len = j - i + 1;
    if (len >= 3) {
      if (d === 0) repeat += len - 1;
      else sequence += len - 1;
    }
    i = j;
  }
  // Repeated chunks ("abcabc", "1212").
  const chunk = /(.{2,}?)\1+/g;
  for (const m of lower.matchAll(chunk)) if (m[0].length >= 4) repeat += m[0].length - m[1].length;
  // Keyboard rows, forwards or backwards, 4 keys or more.
  for (const row of KEYBOARD_ROWS) {
    const rev = [...row].reverse().join("");
    for (const r of [row, rev])
      for (let len = r.length; len >= 4; len--) {
        let found = false;
        for (let s = 0; s + len <= r.length; s++)
          if (lower.includes(r.slice(s, s + len))) {
            sequence += len - 1;
            found = true;
            break;
          }
        if (found) break;
      }
  }
  return { sequence, repeat };
}

/**
 * Score a password from 0 (too weak) to 4 (very strong). `personal` holds the
 * person's name and e-mail words (see `personalWords`), which shouldn't be in it.
 */
export function scorePassword(pw: string, personal: readonly string[] = []): Strength {
  if (!pw) return { score: 0, hints: [] };
  const hints = new Set<PasswordHint>();
  const chars = [...pw];
  const lower = pw.toLowerCase();

  // A common password, maybe with a few digits or symbols tacked on.
  const core = lower.replace(/^[\d\W_]+|[\d\W_]+$/g, "");
  if (COMMON.has(lower) || (core.length >= 4 && COMMON.has(core))) {
    return { score: 0, hints: chars.length < MIN_LENGTH ? ["common", "length"] : ["common"] };
  }

  let effective = chars.length;
  for (const w of COMMON_WORDS)
    if (lower.includes(w)) {
      hints.add("common");
      effective -= w.length - 1;
    }
  for (const w of personal)
    if (w.length >= 3 && lower.includes(w.toLowerCase())) {
      hints.add("personal");
      effective -= w.length - 1;
    }
  const { sequence, repeat } = patternChars(pw);
  if (sequence > 0) hints.add("sequence");
  if (repeat > 0) hints.add("repeat");
  effective -= sequence + repeat;
  // Few different characters ("aabbaabbaabb").
  const distinct = new Set(lower).size;
  effective = Math.min(effective, distinct * 2);
  effective = Math.max(1, effective);

  let pool = 0;
  let classes = 0;
  for (const [re, size] of [[LOWER, 26], [UPPER, 26], [DIGIT, 10], [SYMBOL, 33], [OTHER, 60]] as const)
    if (re.test(pw)) {
      pool += size;
      classes++;
    }
  const bits = effective * Math.log2(Math.max(pool, 2));

  let score: Score = bits < 24 ? 0 : bits < 34 ? 1 : bits < 46 ? 2 : bits < 62 ? 3 : 4;
  if (chars.length < MIN_LENGTH) {
    hints.add("length");
    score = Math.min(score, 2) as Score;
  }
  if (hints.has("common") || hints.has("personal")) score = Math.min(score, 2) as Score;
  if (classes === 1 && score < 4) hints.add("variety");

  const order: PasswordHint[] = ["length", "common", "personal", "sequence", "repeat", "variety"];
  return { score, hints: order.filter((h) => hints.has(h)) };
}
