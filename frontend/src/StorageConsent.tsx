import { createSignal, onCleanup, Show } from "solid-js";
import { api } from "./api";
import { Notice } from "./ui";

interface Authorization {
  authorization_id: string;
  consent_url: string | null;
  state: string;
  status: "pending" | "completed";
  expires_at: string;
}

/** Explicit feature approval. The browser receives only a single-use code. */
export default function StorageConsent(props: { approved: () => void; cancel: () => void }) {
  const [request, setRequest] = createSignal<Authorization>();
  const [code, setCode] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  const retryKey = crypto.randomUUID();
  let mounted = true;
  onCleanup(() => { mounted = false; setCode(""); });
  async function start() {
    if (busy()) return;
    setBusy(true); setError();
    try {
      const value = await api<Authorization>("/api/v1/storage-authorizations", {
        method: "POST", body: {}, headers: { "idempotency-key": retryKey },
      });
      if (mounted) setRequest(value);
    } catch (error) { if (mounted) setError(error); }
    finally { if (mounted) setBusy(false); }
  }
  async function complete() {
    if (busy() || !request() || !code().trim()) return;
    setBusy(true); setError();
    try {
      const value = await api<Authorization>(`/api/v1/storage-authorizations/${request()!.authorization_id}/complete`, {
        method: "POST", body: { code: code().trim(), state: request()!.state },
      });
      if (mounted && value.status === "completed") { setCode(""); props.approved(); }
    } catch (error) { if (mounted) setError(error); }
    finally { if (mounted) setBusy(false); }
  }
  return <section class="storage-consent" aria-label="Briefcase approval">
    <h3>Connect Briefcase</h3>
    <p>Approve access to read your source files and store generated audio. In IAM, choose the account and organization you want to use.</p>
    <Show when={request()} fallback={<button type="button" class="button primary" onClick={start} disabled={busy()}>{busy() ? "Preparing approval…" : "Review Briefcase access"}</button>}>
      <p><a class="text-link" href={request()!.consent_url || undefined} target="_blank" rel="noopener noreferrer">Open approval in IAM ↗</a></p>
      <label>Approval code from IAM
        <input type="password" autocomplete="off" spellcheck={false} value={code()} onInput={(event) => setCode(event.currentTarget.value)} disabled={busy()} placeholder="Paste your approval code" />
      </label>
      <p class="hint">This request expires {new Date(request()!.expires_at).toLocaleTimeString()}. Approval alone does not generate speech.</p>
      <button type="button" class="button primary" onClick={complete} disabled={busy() || !code().trim()}>{busy() ? "Saving approval…" : "Save approval"}</button>
    </Show>
    <button type="button" class="button" onClick={props.cancel} disabled={busy()}>Cancel</button>
    <Notice error={error()} />
  </section>;
}
