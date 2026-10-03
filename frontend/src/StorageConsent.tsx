import { Show, createSignal, onCleanup } from "solid-js";
import { ApiError, bindApi, session } from "./api";
import { Notice } from "./ui";
import { openIamPopup } from "./iam-popup";
import { createLoginCancellation, createLoginLifecycle } from "./login-lifecycle";
import { storageAuthorization, type StorageAuthorization } from "./storage-authorization";

type Review = StorageAuthorization & { redirect_url?: string; manual?: boolean; code_saved?: boolean };
/** Approval is explicit and resumable. Dismissing this UI never revokes a grant. */
export default function StorageConsent(props: { approved: () => void; cancel: () => void }) {
  const api = bindApi(), context = session()?.context;
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal<unknown>();
  const [review, setReview] = createSignal<Review>(), [code, setCode] = createSignal("");
  const [hasRetryCode, setHasRetryCode] = createSignal(false);
  let retryCode: string | undefined;
  const lifecycle = createLoginLifecycle(setBusy);
  const current = () => session()?.context === context;
  const cancellation = createLoginCancellation(
    () => session()?.context,
    async (review_nonce, before) => {
      await api("/api/session/storage/cancel", { method: "POST", body: { review_nonce } });
      return { context: before || "" };
    },
    () => {},
  );
  function stop() { return { fence: lifecycle.cancel(), done: cancellation.cancel() }; }
  function dismiss() {
    const stopped = stop(); setCode(""); retryCode = undefined; setHasRetryCode(false);
    void stopped.done.then(() => { if (stopped.fence.current() && current()) props.cancel(); })
      .catch(err => { if (stopped.fence.current() && current()) setError(err); });
  }
  onCleanup(() => { retryCode = undefined; const stopped = stop(); lifecycle.dispose(); void stopped.done.catch(() => {}); });
  const nonce = () => Array.from(crypto.getRandomValues(new Uint8Array(32)), value => value.toString(16).padStart(2, "0")).join("");
  function approved() { retryCode = undefined; setHasRetryCode(false); cancellation.forget(); props.approved(); }
  function failed(err: unknown) {
    if (err instanceof ApiError && (err.status === 412 || ["reconsent_required", "approval_expired", "invalid_storage_authorization", "storage_authorization_expired"].includes(err.code))) { setReview(); retryCode = undefined; setHasRetryCode(false); }
    setError(err);
  }
  async function prepare(popup: boolean) {
    const stopped = stop(), attempt = lifecycle.begin(); setError();
    // A blocked popup never invokes its start callback; still observe cancellation failure.
    void stopped.done.catch(() => {});
    const begin = async (popupNonce?: string) => {
      await stopped.done;
      if (!attempt.current() || !current()) throw new Error("Approval was cancelled.");
      const reviewNonce = nonce(); cancellation.remember(reviewNonce);
      const value = await api<Review>("/api/session/storage/start", { method: "POST", body: { review_nonce: reviewNonce, ...(popupNonce ? { popup_nonce: popupNonce } : {}) }, signal: attempt.signal });
      storageAuthorization(value);
      if (!attempt.current() || !current()) throw new Error("Approval was cancelled.");
      setReview(value);
      return value.redirect_url || value.consent_url || "/";
    };
    try {
      if (popup) {
        await openIamPopup(begin, attempt.signal);
        if (!attempt.current() || !current()) return;
        await check(attempt);
      } else {
        await begin();
        if (attempt.current() && current() && review()?.status === "completed") approved();
      }
    } catch (err) { if (attempt.current() && current()) failed(err); }
    finally { attempt.finish(); }
  }
  async function check(attempt = lifecycle.begin()) {
    const expected = review(); if (!expected) { attempt.finish(); return; }
    try {
      const value = await api<Review>("/api/session/storage/status", { method: "POST", body: { authorization_id: expected.authorization_id, state: expected.state }, signal: attempt.signal });
      storageAuthorization(value, expected);
      if (!attempt.current() || !current()) return;
      setReview(value);
      if (value.status === "completed") approved();
      else setError(new Error("Approval is still pending. Finish reviewing in IAM, or paste its one-use code."));
    } catch (err) { if (attempt.current() && current()) failed(err); }
    finally { attempt.finish(); }
  }
  async function complete() {
    const expected = review(); if (!expected) return;
    const attempt = lifecycle.begin(), supplied = code().trim() || retryCode;
    retryCode = supplied; setHasRetryCode(!!retryCode); setCode(""); setError();
    try {
      const value = await api<Review>("/api/session/storage/complete", { method: "POST", body: { authorization_id: expected.authorization_id, state: expected.state, ...(supplied ? { code: supplied } : {}) }, signal: attempt.signal });
      storageAuthorization(value, expected);
      if (!attempt.current() || !current()) return;
      if (value.status !== "completed") throw new Error("Approval is still pending.");
      approved();
    } catch (err) {
      if (attempt.current() && current()) failed(err);
    } finally { attempt.finish(); }
  }
  return <section class="storage-consent" aria-label="Briefcase approval">
    <h3>Connect Briefcase</h3>
    <p>Approve access to read your source files and store generated audio. In IAM, choose the account and organization you want to use.</p>
    <button type="button" class="button primary" onClick={() => void prepare(true)} disabled={busy()}>{busy() ? "Waiting for your approval…" : "Review in popup"}</button>
    <button type="button" class="button" onClick={() => void prepare(false)}>Review manually</button>
    <Show when={review()}>{value => <div>
      <Show when={value().consent_url}><p><a class="button" href={value().consent_url!} target="_blank" rel="noopener noreferrer">Open IAM review in another tab</a></p></Show>
      <p class="hint">Keep this page open to preserve your speech draft. {value().manual ? "Copy the one-use approval code from IAM." : "After reviewing, return here and check approval, or paste the one-use code if IAM provides one."}</p>
      <label>Approval code<input type="password" autocomplete="off" value={code()} onInput={event => setCode(event.currentTarget.value)} placeholder="obc_…" /></label>
      <button type="button" class="button primary" onClick={() => void complete()} disabled={busy() || (!code().trim() && !value().code_saved && !hasRetryCode())}>{(value().code_saved || hasRetryCode()) && !code().trim() ? "Retry completion" : "Complete approval"}</button>
      <button type="button" class="button" onClick={() => void check()} disabled={busy()}>Check approval</button>
    </div>}</Show>
    <p class="hint">No speech is generated until you explicitly retry the original request.</p>
    <button type="button" class="button" onClick={dismiss}>Cancel review</button>
    <Notice error={error()} />
  </section>;
}
