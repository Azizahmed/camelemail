import { describe, expect, it } from "vitest";
import { MIN_LENGTH, personalWords, scorePassword } from "./password";

describe("password strength", () => {
  it("rates nothing as nothing", () => {
    expect(scorePassword("")).toEqual({ score: 0, hints: [] });
  });

  it("knows common passwords, also with digits tacked on", () => {
    expect(scorePassword("password").score).toBe(0);
    expect(scorePassword("Password123!").score).toBe(0);
    expect(scorePassword("qwerty123").hints).toContain("common");
    expect(scorePassword("iloveyou").score).toBe(0);
  });

  it("caps short passwords below what the server accepts", () => {
    const s = scorePassword("Xk#9q!Lz");
    expect(s.score).toBeLessThanOrEqual(2);
    expect(s.hints[0]).toBe("length");
    expect(MIN_LENGTH).toBe(10);
  });

  it("marks sequences and repeats", () => {
    const seq = scorePassword("abcdefghijkl");
    expect(seq.hints).toContain("sequence");
    expect(seq.score).toBeLessThanOrEqual(1);
    const rep = scorePassword("aaaaaaaaaaaa");
    expect(rep.hints).toContain("repeat");
    expect(rep.score).toBe(0);
    expect(scorePassword("asdfghjkl123").hints).toContain("sequence");
    expect(scorePassword("xyxyxyxyxyxy").hints).toContain("repeat");
  });

  it("marks the person's name and e-mail in the password", () => {
    const words = personalWords("Aziz Ahmed", "aziz.ahmed@example.com");
    expect(words).toEqual(expect.arrayContaining(["aziz", "ahmed", "aziz.ahmed"]));
    const s = scorePassword("Aziz!Ahmed2026", words);
    expect(s.hints).toContain("personal");
    expect(s.score).toBeLessThanOrEqual(2);
  });

  it("rates long, varied passwords as strong", () => {
    expect(scorePassword("kq7#Lp2vZ9!m").score).toBe(4);
    expect(scorePassword("correct horse battery staple").score).toBeGreaterThanOrEqual(3);
    expect(scorePassword("Tr0ub4dor&3x").score).toBeGreaterThanOrEqual(3);
    expect(scorePassword("kq7#Lp2vZ9!m").hints).toEqual([]);
  });

  it("asks for variety in single-kind passwords that aren't long", () => {
    expect(scorePassword("mzqtrwplkx").hints).toContain("variety");
  });

  it("counts Arabic letters", () => {
    expect(scorePassword("كلمة سر طويلة جدا").score).toBeGreaterThanOrEqual(3);
  });

  it("only goes up as the password grows from a strong start", () => {
    let prev = 0;
    const full = "kq7#Lp2vZ9!mW4";
    for (let i = 10; i <= full.length; i++) {
      const s = scorePassword(full.slice(0, i)).score;
      expect(s).toBeGreaterThanOrEqual(prev);
      prev = s;
    }
  });
});
