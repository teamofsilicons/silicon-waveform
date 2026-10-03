import test from "node:test";
import assert from "node:assert/strict";
import { storageAuthorization } from "../src/storage-authorization.ts";
const request = {
  authorization_id: "11111111-1111-4111-8111-111111111111",
  consent_url: "https://iam.example/approve",
  state: "private-state",
  status: "pending",
  expires_at: new Date(Date.now() + 600000).toISOString(),
};
test("Briefcase approval validates links and binds the completed response to its pending request", () => {
  assert.equal(storageAuthorization(request), request);
  for (const consent_url of [
    "javascript:alert(1)",
    "http://example.com",
    "https://user:pass@example.com",
  ])
    assert.throws(() => storageAuthorization({ ...request, consent_url }));
  assert.throws(() =>
    storageAuthorization({ ...request, expires_at: "2000-01-01" }),
  );
  assert.throws(() =>
    storageAuthorization(
      { ...request, status: "completed", state: "foreign-state" },
      request,
    ),
  );
  assert.throws(() =>
    storageAuthorization(
      {
        ...request,
        status: "completed",
        authorization_id: "22222222-2222-4222-8222-222222222222",
      },
      request,
    ),
  );
  assert.equal(
    storageAuthorization(
      { ...request, status: "completed", consent_url: null },
      request,
    ).status,
    "completed",
  );
});
