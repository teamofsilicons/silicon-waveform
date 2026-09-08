import { createResource, createSignal, For, Show } from "solid-js";
import {
  api,
  date,
  session,
  sessionAction,
  setContextVersion,
  type Environment,
} from "./api";
import { Copy, Empty, Heading, Icon, Modal, Notice } from "./ui";
export default function Environments(props: {
  signin: () => void;
  connect: () => void;
}) {
  const [environments, { refetch }] = createResource(
    () => session()?.authenticated && session()?.plane === "production",
    () => api<{ items: Environment[] }>("/api/v1/testing-environments"),
  );
  const [creating, setCreating] = createSignal(false),
    [detail, setDetail] = createSignal<Environment>(),
    [revealed, setRevealed] = createSignal<{ key: string; name: string }>(),
    [confirm, setConfirm] = createSignal<{
      action: string;
      environment?: Environment;
    }>();
  const [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>(),
    [notice, setNotice] = createSignal("");
  const [name, setName] = createSignal(""),
    [description, setDescription] = createSignal(""),
    [iamId, setIamId] = createSignal(""),
    [iamKey, setIamKey] = createSignal(""),
    [appSecret, setAppSecret] = createSignal(""),
    [briefcaseKey, setBriefcaseKey] = createSignal("");
  function clearForm() {
    setName("");
    setDescription("");
    setIamId("");
    setIamKey("");
    setAppSecret("");
    setBriefcaseKey("");
  }
  async function create(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setError();
    try {
      const value = await api<Environment & { key: string }>(
        "/api/v1/testing-environments",
        {
          method: "POST",
          body: {
            name: name().trim(),
            description: description().trim() || null,
            iam_environment_id: iamId().trim(),
            iam_environment_key: iamKey().trim(),
            app_secret: appSecret().trim(),
            briefcase_environment_key: briefcaseKey().trim(),
          },
        },
      );
      setCreating(false);
      clearForm();
      setRevealed({ key: value.key, name: value.name });
      await refetch();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function inspect(environment: Environment) {
    setError();
    try {
      setDetail(
        await api<Environment>(
          `/api/v1/testing-environments/${environment.id}`,
        ),
      );
    } catch (err) {
      setError(err);
    }
  }
  async function reveal(environment: Environment) {
    setError();
    setBusy(true);
    try {
      const value = await api<{ key: string }>(
        `/api/v1/testing-environments/${environment.id}/key`,
      );
      setRevealed({ key: value.key, name: environment.name });
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function act() {
    const pending = confirm();
    if (!pending) return;
    setBusy(true);
    setError();
    setNotice("");
    try {
      const path =
        pending.action === "clean"
          ? "/api/v1/testing-environment/clean"
          : `/api/v1/testing-environments/${pending.environment!.id}/${pending.action}`;
      const value = await api<{ key?: string }>(path, {
        method: "POST",
        body: {},
      });
      setConfirm();
      setDetail();
      if (value.key)
        setRevealed({ key: value.key, name: pending.environment!.name });
      else
        setNotice(
          pending.action === "delete"
            ? "Environment deleted. You can restore it within 30 days."
            : pending.action === "restore"
              ? "Environment restored."
              : "The test environment’s Waveform data has been cleared.",
        );
      await refetch();
      if (pending.action === "clean") setContextVersion((v) => v + 1);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  const active = () =>
    !environments.error
      ? environments()?.items.filter((e) => !e.deleted_at) || []
      : [];
  const deleted = () =>
    !environments.error
      ? environments()?.items.filter((e) => e.deleted_at) || []
      : [];
  return (
    <>
      <Heading
        eyebrow="DEVELOP & TEST"
        title="Testing environments"
        action={
          <div class="actions">
            <button class="button" onClick={props.connect}>
              Connect with a key
            </button>
            <Show
              when={
                session()?.authenticated && session()?.plane === "production"
              }
            >
              <button
                class="button primary"
                onClick={() => {
                  clearForm();
                  setError();
                  setCreating(true);
                }}
              >
                Create environment
              </button>
            </Show>
          </div>
        }
      >
        A separate space to try everything.
      </Heading>
      <Notice error={error() || environments.error} message={notice()} />
      <Show
        when={session()?.plane === "test"}
        fallback={
          <>
            <Show
              when={session()?.authenticated}
              fallback={
                <section class="panel">
                  <Empty icon="test" title="A safe place to experiment">
                    <p>
                      Sign in to manage your organization’s environments, or
                      connect with an existing key.
                    </p>
                    <button class="button primary" onClick={props.signin}>
                      Sign in with IAM
                    </button>
                  </Empty>
                </section>
              }
            >
              <section class="panel">
                <div class="panel-heading">
                  <h2>Active environments</h2>
                  <span class="mono muted">{active().length}</span>
                </div>
                <Show
                  when={active().length}
                  fallback={
                    <Empty
                      icon="test"
                      title={
                        environments.loading
                          ? "Loading environments…"
                          : "Room to experiment"
                      }
                    >
                      Create an environment linked to IAM and Briefcase test
                      environments.
                    </Empty>
                  }
                >
                  <div class="environment-list">
                    <For each={active()}>
                      {(environment) => (
                        <div class="environment-row">
                          <div class="environment-symbol">
                            <Icon name="test" />
                          </div>
                          <div class="grow">
                            <button
                              class="job-link"
                              onClick={() => void inspect(environment)}
                            >
                              <strong>{environment.name}</strong>
                            </button>
                            <p class="hint">
                              {environment.description ||
                                "Isolated Waveform environment"}
                            </p>
                            <small class="muted">
                              Last active {date(environment.last_activity_at)}
                            </small>
                          </div>
                          <span class="badge completed">Active</span>
                          <button
                            class="button"
                            disabled={busy()}
                            onClick={() => void reveal(environment)}
                          >
                            View key
                          </button>
                          <button
                            class="icon-button"
                            aria-label={`Manage ${environment.name}`}
                            onClick={() => void inspect(environment)}
                          >
                            <Icon name="chevron" />
                          </button>
                        </div>
                      )}
                    </For>
                  </div>
                </Show>
              </section>
              <Show when={deleted().length}>
                <section class="panel">
                  <div class="panel-heading">
                    <h2>Recently deleted</h2>
                    <span class="hint">Recoverable for 30 days</span>
                  </div>
                  <div class="environment-list">
                    <For each={deleted()}>
                      {(environment) => (
                        <div class="environment-row">
                          <div class="grow">
                            <strong>{environment.name}</strong>
                            <p class="hint">
                              Deleted {date(environment.deleted_at)}
                            </p>
                          </div>
                          <button
                            class="button"
                            disabled={busy()}
                            onClick={() =>
                              setConfirm({ action: "restore", environment })
                            }
                          >
                            Restore
                          </button>
                        </div>
                      )}
                    </For>
                  </div>
                </section>
              </Show>
            </Show>
          </>
        }
      >
        <section class="panel settings-panel">
          <div class="panel-heading">
            <div>
              <span class="badge test">Connected test environment</span>
              <h2 class="spaced-title">{session()?.environment?.name}</h2>
              <p class="hint">
                {session()?.environment?.description ||
                  "Your isolated Waveform workspace."}
              </p>
            </div>
            <Icon name="test" size={32} />
          </div>
          <dl class="details">
            <dt>Environment ID</dt>
            <dd class="mono">{session()?.environment?.id}</dd>
            <dt>Created</dt>
            <dd>{date(session()?.environment?.created_at)}</dd>
            <dt>Speech behavior</dt>
            <dd>Shared TTS and STT fixtures; no paid generation.</dd>
            <dt>Storage</dt>
            <dd>
              Each TTS result is uploaded to the linked Briefcase test
              environment.
            </dd>
          </dl>
          <div class="panel-footer">
            <button
              class="button"
              onClick={() =>
                void sessionAction("switch", { plane: "production" }).catch(
                  setError,
                )
              }
            >
              Back to production
            </button>
            <button
              class="button danger-button"
              onClick={() => setConfirm({ action: "clean" })}
            >
              Clean environment
            </button>
          </div>
        </section>
      </Show>
      <div class="info-strip">
        <Icon name="test" />
        <p>
          Test requests stay isolated from production IAM and Briefcase.
          Inactive environments are deleted after 15 days and remain recoverable
          for 30 days.
        </p>
      </div>
      <Show when={creating()}>
        <Modal
          title="Create testing environment"
          wide
          close={() => {
            if (!busy()) {
              setCreating(false);
              clearForm();
              setError();
            }
          }}
        >
          <form class="stack" onSubmit={create}>
            <p class="muted">
              Connect the matching IAM and Briefcase test environments. Use the
              Waveform app secret from that IAM test environment.
            </p>
            <div class="two-columns">
              <label>
                Name
                <input
                  required
                  maxlength={128}
                  pattern="[^/]+"
                  value={name()}
                  onInput={(e) => setName(e.currentTarget.value)}
                  placeholder="e.g. Speech experiments"
                />
              </label>
              <label>
                Description <span class="label-note">optional</span>
                <input
                  value={description()}
                  onInput={(e) => setDescription(e.currentTarget.value)}
                />
              </label>
            </div>
            <label>
              IAM environment ID
              <input
                required
                pattern="[a-fA-F0-9-]{36}"
                value={iamId()}
                onInput={(e) => setIamId(e.currentTarget.value)}
                placeholder="Environment UUID"
              />
            </label>
            <label>
              IAM environment key
              <input
                required
                type="password"
                autocomplete="off"
                pattern="[A-Za-z0-9]{32}"
                value={iamKey()}
                onInput={(e) => setIamKey(e.currentTarget.value)}
                placeholder="32-character key"
              />
            </label>
            <label>
              Waveform test application secret
              <input
                required
                type="password"
                autocomplete="off"
                maxlength={512}
                value={appSecret()}
                onInput={(e) => setAppSecret(e.currentTarget.value)}
              />
            </label>
            <label>
              Briefcase environment key
              <input
                required
                type="password"
                autocomplete="off"
                pattern="[A-Za-z0-9]{32}"
                value={briefcaseKey()}
                onInput={(e) => setBriefcaseKey(e.currentTarget.value)}
                placeholder="32-character key"
              />
            </label>
            <Notice error={error()} />
            <button class="button primary" disabled={busy()}>
              {busy() ? "Creating…" : "Create environment"}
            </button>
          </form>
        </Modal>
      </Show>
      <Show when={detail()}>
        {(environment) => (
          <Modal title={environment().name} close={() => setDetail()}>
            <div class="stack">
              <p class="muted">
                {environment().description ||
                  "An isolated Waveform testing environment."}
              </p>
              <dl class="details">
                <dt>ID</dt>
                <dd class="mono">{environment().id}</dd>
                <dt>Created</dt>
                <dd>{date(environment().created_at)}</dd>
                <dt>Last active</dt>
                <dd>{date(environment().last_activity_at)}</dd>
              </dl>
              <Notice error={error()} />
              <div class="actions">
                <button
                  class="button"
                  disabled={busy()}
                  onClick={() => {
                    const e = environment();
                    setDetail();
                    void reveal(e);
                  }}
                >
                  View key
                </button>
                <button
                  class="button"
                  onClick={() => {
                    const e = environment();
                    setDetail();
                    setConfirm({ action: "rotate-key", environment: e });
                  }}
                >
                  Rotate key
                </button>
                <button
                  class="button danger-button"
                  onClick={() => {
                    const e = environment();
                    setDetail();
                    setConfirm({ action: "delete", environment: e });
                  }}
                >
                  Delete environment
                </button>
              </div>
              <p class="hint">
                Key access and management require the creator, an organization
                admin, or owner. The backend checks your permissions.
              </p>
            </div>
          </Modal>
        )}
      </Show>
      <Show when={revealed()}>
        {(value) => (
          <Modal
            title={`${value().name} · Access key`}
            close={() => setRevealed()}
          >
            <div class="stack">
              <p class="muted">
                Anyone with this key can access this testing environment. Share
                it only with your testing team.
              </p>
              <label>
                Waveform environment key
                <input type="password" readonly value={value().key} />
              </label>
              <Copy value={value().key} label="Copy environment key" />
              <Notice error={error()} />
              <button
                class="button primary"
                disabled={busy()}
                onClick={async () => {
                  setBusy(true);
                  try {
                    await sessionAction("environment", {
                      key: value().key,
                      org: session()?.org,
                    });
                    setRevealed();
                  } catch (err) {
                    setError(err);
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                Use this environment
              </button>
            </div>
          </Modal>
        )}
      </Show>
      <Show when={confirm()}>
        {(pending) => (
          <Modal
            title={
              pending().action === "clean"
                ? "Clean this environment?"
                : pending().action === "rotate-key"
                  ? "Rotate the access key?"
                  : pending().action === "restore"
                    ? "Restore this environment?"
                    : "Delete this environment?"
            }
            close={() => !busy() && setConfirm()}
          >
            <div class="stack">
              <p>
                {pending().action === "clean"
                  ? "This permanently clears Waveform jobs, preferences, provider keys, and request history in this test environment. The environment and its key remain. Files already uploaded to Briefcase remain there."
                  : pending().action === "rotate-key"
                    ? "The old key will stop working immediately. Update any clients using it."
                    : pending().action === "restore"
                      ? "The environment will become accessible again with its existing key."
                      : "The environment will become inaccessible. You can restore it within 30 days."}
              </p>
              <Notice error={error()} />
              <button
                class={`button ${pending().action === "restore" ? "primary" : "danger-button"}`}
                disabled={busy()}
                onClick={() => void act()}
              >
                {busy()
                  ? "Working…"
                  : pending().action === "clean"
                    ? "Clear test data"
                    : pending().action === "rotate-key"
                      ? "Rotate key"
                      : pending().action === "restore"
                        ? "Restore environment"
                        : "Delete environment"}
              </button>
            </div>
          </Modal>
        )}
      </Show>
    </>
  );
}
