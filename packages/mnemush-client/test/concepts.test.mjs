import { test } from "node:test";
import assert from "node:assert";
// Node >= 23 strips types natively; the test imports the TS source directly.
import { buildConceptInject, parseConceptsJson } from "../src/index.ts";

test("buildConceptInject formats concept table", () => {
  const inject = buildConceptInject([
    { title: "GitHub proxy setup", category: "lesson", importance: 0.9, score: 1.2 },
    { title: "FTS rowid 陷阱", category: "lesson", importance: 0.8, score: 1.1 },
  ]);
  assert.match(inject, /\[memory index\] 2 concepts/);
  assert.match(inject, /· GitHub proxy setup \(lesson\)/);
  assert.match(inject, /· FTS rowid 陷阱 \(lesson\)/);
  assert.match(inject, /detail via memory tool/);
});

test("buildConceptInject returns empty string for empty array", () => {
  assert.equal(buildConceptInject([]), "");
});

test("parseConceptsJson accepts spec shape and bare array", () => {
  // spec 形状: {"concepts": [...], "count": N}
  const specShape = parseConceptsJson(
    '{"concepts":[{"title":"a","category":"note","importance":0.9,"score":1.2}],"count":1}',
  );
  assert.equal(specShape.length, 1);
  assert.equal(specShape[0].title, "a");
  assert.equal(specShape[0].category, "note");
  // 旧版裸数组兼容
  const bare = parseConceptsJson('[{"title":"b","category":"lesson","importance":0.8,"score":1.1}]');
  assert.equal(bare.length, 1);
  assert.equal(bare[0].title, "b");
  // 无效输入 → []
  assert.equal(parseConceptsJson("not json").length, 0);
  assert.equal(parseConceptsJson("{}").length, 0);
});
