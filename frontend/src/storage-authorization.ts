export interface StorageAuthorization {
  authorization_id: string;
  consent_url: string | null;
  state: string;
  status: "pending" | "completed";
  expires_at: string;
}

export function storageAuthorization(
  value: unknown,
  expected?: StorageAuthorization,
): StorageAuthorization {
  const data = value as StorageAuthorization;
  if (
    !data ||
    !/^[a-f0-9-]{36}$/.test(data.authorization_id) ||
    typeof data.state !== "string" ||
    !data.state ||
    !["pending", "completed"].includes(data.status) ||
    !Number.isFinite(Date.parse(data.expires_at)) ||
    (expected &&
      (data.authorization_id !== expected.authorization_id ||
        data.state !== expected.state))
  )
    throw new Error(
      "The Briefcase approval response did not match this request.",
    );
  if (data.status === "pending") {
    const url = new URL(data.consent_url || "");
    if (url.protocol !== "https:" || url.username || url.password)
      throw new Error("IAM returned an invalid approval link.");
    if (Date.parse(data.expires_at) <= Date.now())
      throw new Error("This approval expired. Start a new permission review.");
  }
  return data;
}
