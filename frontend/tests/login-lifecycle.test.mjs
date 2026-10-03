import test from "node:test";
import assert from "node:assert/strict";
import { createLoginLifecycle, createLoginCancellation } from "../src/login-lifecycle.ts";

test("closing a pending popup clears only login busy and stale completion cannot unlock a reopened attempt", () => {
  let busy = false;
  const lifecycle = createLoginLifecycle(value => { busy = value; });
  const first = lifecycle.begin();
  assert.equal(busy, true);
  lifecycle.cancel();
  assert.equal(busy, false); assert.equal(first.signal.aborted, true);
  const second = lifecycle.begin(); first.finish();
  assert.equal(busy, true); assert.equal(first.current(), false);
  lifecycle.cancel(); assert.equal(busy, false); assert.equal(second.signal.aborted, true);
  lifecycle.dispose(); assert.equal(second.current(), false);
});

test("transient cancellation failure retains the exact nonce for full-page retry", async () => {
  let context = "before", fail = true; const calls = [];
  const receipt = createLoginCancellation(() => context, async (nonce, selected) => {
    calls.push({ nonce, selected });
    if (fail) throw new Error("upstream 503");
    return { context: "restored" };
  }, value => { context = value.context; });
  receipt.remember("nonce");
  await assert.rejects(receipt.cancel(), /503/);
  fail = false; assert.equal(await receipt.cancel(), "restored");
  assert.deepEqual(calls, [{nonce:"nonce",selected:"before"},{nonce:"nonce",selected:"before"}]);
  assert.equal(context, "restored"); assert.equal(await receipt.cancel(), "restored"); assert.equal(calls.length, 2);
});

test("Close then reopen/full-page shares cancellation and adopts its result before continuing", async () => {
  let context = "before", resolve, calls = 0;
  const receipt = createLoginCancellation(() => context, async () => { calls++; return new Promise(done => { resolve = done; }); }, value => { context = value.context; });
  receipt.remember("nonce");
  const close = receipt.cancel(), fallback = receipt.cancel();
  assert.equal(close, fallback);
  await Promise.resolve(); resolve({context:"restored"});
  assert.equal(await fallback, context); assert.equal(context,"restored"); assert.equal(calls,1);
});

test("cancellation response cannot replace an intervening organization selection", async () => {
  let context = "before", resolve;
  const receipt = createLoginCancellation(() => context, async () => new Promise(done => { resolve = done; }), value => { context = value.context; });
  receipt.remember("nonce"); const pending = receipt.cancel(); await Promise.resolve();
  context = "other-org"; resolve({context:"restored"}); await pending;
  assert.equal(context,"other-org");
});
