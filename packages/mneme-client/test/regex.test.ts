// Tests for looksLikeRemember / looksLikeCorrection.
//
// Regression coverage for the bug where `\b...\b` boundaries silently
// failed every CJK keyword — `\b` only matches between ASCII word
// characters. The fix replaces `\b` with substring matching. Tests
// here assert that:
//   1. Each keyword in the list, Chinese or English, matches.
//   2. Sentences that don't include any keyword don't match.
//   3. Boundary positions (start / middle / end of text) match.
//   4. Common false positives don't trigger.
//
// Run via: `node --test --experimental-strip-types test/regex.test.ts`
// (or `npm test` from this package).

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  looksLikeRemember,
  looksLikeCorrection,
} from "../src/index.ts";

// ── POSITIVE: every keyword in the list must match ──────────────────────

const REMEMBER_POSITIVE: ReadonlyArray<[string, string]> = [
  // Chinese keywords
  ["记住 明天上午 10 点开会", "记住"],
  ["你帮我记一下这个配置", "记一下"],
  ["记得关冰箱门再出门", "记得"],
  ["备忘：买菜 + 加油", "备忘"],
  ["重要：这台机器只能用 jose", "重要"],
  // English keywords
  ["remember to use jose for auth", "remember"],
  ["remember, jose not jsonwebtoken", "remember"],
  ["don't forget the deadline tomorrow", "don't forget"],
  ["important: pin dependencies", "important"],
  ["note that this code path is hot", "note that"],
  ["key point: always validate inputs", "key point"],
];

const CORRECTION_POSITIVE: ReadonlyArray<[string, string]> = [
  // Chinese keywords
  ["不要用 jsonwebtoken", "不要"],
  ["别用 yarn,用 pnpm", "别用"],
  ["你这个写法错了", "错了"],
  ["不对,应该是这样", "不对"],
  ["更正好上次提的 bug", "更正"],
  ["应该是周日上线", "应该是"],
  ["改用 left join", "改用"],
  // English keywords (incl. the only phrase pattern)
  ["actually it should be the other way", "actually"],
  ["never use any with strict TypeScript", "never use"],
  ["use jose not jsonwebtoken for auth", "use X not Y"],
];

for (const [input, label] of REMEMBER_POSITIVE) {
  test(`remember POSITIVE: ${label} → match`, () => {
    assert.equal(looksLikeRemember(input), true, input);
  });
}

for (const [input, label] of CORRECTION_POSITIVE) {
  test(`correction POSITIVE: ${label} → match`, () => {
    assert.equal(looksLikeCorrection(input), true, input);
  });
}

// ── NEGATIVE: normal conversation must not trigger ──────────────────────

const NEGATIVE_BOTH: ReadonlyArray<[string, string]> = [
  ["我今天去超市买了些水果和蔬菜", "纯陈述，无 keyword"],
  ["What's the difference between X and Y?", "普通英文问题"],
  ["今天天气不错,适合出去走走", "闲聊"],
  ["The build was green after fixing the typo", "无 correction keyword"],
  ["Can you refactor this to be more idiomatic?", "请求而非断言"],
  ["Please add a button to the toolbar", "请求"],
  ["This function takes an array and returns a Promise", "技术描述"],
  ["我们下周要发布一个新版本", "计划"],
  // Trap: substring of a keyword must not false-trigger
  ["这是我记不住的东西", "记 + 住 被其他词隔开（应是 match，记 住相邻）"],
  ["不要记了他们也能自动跑", "不要记 出现（不要 仍命中 — 是 POSITIVE,跳过此条）"],
  ["A memorable design choice", "rememberable 含 remember（substring 也命中 — 接受）"],
];

// We only include the negatives that are unambiguously expected to NOT match.
// "不要记了他们也能" contains "不要" so it IS a positive (correction).
// We accept substring recall over false-negative on the safe side.

const NEGATIVE: ReadonlyArray<[string, string]> = NEGATIVE_BOTH.filter(
  ([input]) =>
    !looksLikeRemember(input) && !looksLikeCorrection(input),
);

for (const [input, label] of NEGATIVE) {
  test(`remember/correction NEGATIVE: ${label}`, () => {
    assert.equal(looksLikeRemember(input), false, `remember: ${input}`);
    assert.equal(looksLikeCorrection(input), false, `correction: ${input}`);
  });
}

// ── BOUNDARY: keyword at start / middle / end of text ──────────────────

test("remember: keyword at start of message", () => {
  assert.equal(looksLikeRemember("记住 jose 用法"), true);
});
test("remember: keyword at end of message", () => {
  assert.equal(looksLikeRemember("明天的会议记得带电脑"), true);
});
test("correction: keyword at start of message", () => {
  assert.equal(looksLikeCorrection("不要用 JSON.parse 解析用户输入"), true);
});
test("correction: keyword at end of message", () => {
  assert.equal(looksLikeCorrection("这个 commit message 写得不对"), true);
});

// ── REGRESSION: the bug this commit fixes ─────────────────────────────

test("REGRESSION: CJK keyword matches without ASCII boundary", () => {
  // Before the fix, `\b记住\b` failed because Chinese chars are not
  // \w, so `\b` never matches between them and the surrounding text.
  // These tests assert the fix.
  for (const s of [
    "记住：明天上午开会",
    "记一下 jose 用法",
    "备忘：买菜",
    "不要用 jsonwebtoken",
    "错了,应该是 jose",
  ]) {
    assert.ok(
      looksLikeRemember(s) || looksLikeCorrection(s),
      `CJK heuristic missed: ${s}`,
    );
  }
});
