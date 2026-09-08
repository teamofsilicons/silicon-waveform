import { createEffect, createSignal, For, onCleanup, Show } from "solid-js";
import { api, date, duration, providerNames, session, type Job } from "./api";
import { Copy, Empty, Heading, Modal, Notice } from "./ui";
export default function History(props: { signin: () => void }) {
  const [jobs, setJobs] = createSignal<Job[]>([]),
    [filter, setFilter] = createSignal(""),
    [cursor, setCursor] = createSignal<string | null>(null),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>(),
    [detail, setDetail] = createSignal<Job>();
  let generation = 0,
    mounted = true;
  async function load(more = false, quiet = false) {
    if (!session()?.authenticated) return;
    const version = ++generation;
    if (!quiet) setBusy(true);
    setError();
    const params = new URLSearchParams({ limit: "20" });
    if (filter()) params.set("operation", filter());
    if (more && cursor()) params.set("cursor", cursor()!);
    try {
      const page = await api<{ items: Job[]; next_cursor: string | null }>(
        `/api/v1/jobs?${params}`,
      );
      if (mounted && version === generation) {
        setJobs((old) => (more ? [...old, ...page.items] : page.items));
        setCursor(page.next_cursor);
      }
    } catch (err) {
      if (mounted && version === generation) setError(err);
    } finally {
      if (mounted && version === generation) setBusy(false);
    }
  }
  createEffect(() => {
    filter();
    void load();
  });
  const timer = setInterval(async () => {
    if (!session()?.authenticated || document.hidden || busy()) return;
    if (detail()?.status === "running") {
      try {
        const next = await api<Job>(`/api/v1/jobs/${detail()!.id}`);
        if (mounted && detail()?.id === next.id) setDetail(next);
      } catch (err) {
        if (mounted) setError(err);
      }
    }
    // Refresh only the loaded records so polling never discards pagination.
    for (const job of jobs().filter((j) => j.status === "running")) {
      try {
        const next = await api<Job>(`/api/v1/jobs/${job.id}`);
        if (mounted)
          setJobs((rows) =>
            rows.map((row) => (row.id === next.id ? next : row)),
          );
      } catch (err) {
        if (mounted) setError(err);
        break;
      }
    }
  }, 4000);
  onCleanup(() => {
    mounted = false;
    clearInterval(timer);
  });
  async function open(job: Job) {
    setError();
    try {
      setDetail(await api<Job>(`/api/v1/jobs/${job.id}`));
    } catch (err) {
      setError(err);
    }
  }
  return (
    <>
      <Heading
        eyebrow="YOUR ACTIVITY"
        title="Job history"
        action={
          <button
            class="button"
            onClick={() => void load()}
            disabled={busy() || !session()?.authenticated}
          >
            Refresh
          </button>
        }
      >
        Every request, from the first word to the final result.
      </Heading>
      <section class="panel">
        <div class="panel-heading">
          <h2>Your jobs</h2>
          <label class="filter-label">
            <span class="sr-only">Filter by operation</span>
            <select
              value={filter()}
              onChange={(e) => setFilter(e.currentTarget.value)}
            >
              <option value="">All operations</option>
              <option value="tts">Text to speech</option>
              <option value="stt">Speech to text</option>
            </select>
          </label>
        </div>
        <Notice error={error()} />
        <Show
          when={session()?.authenticated}
          fallback={
            <Empty title="Your activity belongs to you" icon="history">
              <button class="button primary" onClick={props.signin}>
                Sign in to view jobs
              </button>
            </Empty>
          }
        >
          <Show
            when={jobs().length}
            fallback={
              <Empty
                icon="history"
                title={busy() ? "Loading your jobs…" : "No jobs yet"}
              >
                Generate speech or transcribe a file to start your history.
              </Empty>
            }
          >
            <div class="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>Job</th>
                    <th>Status</th>
                    <th>Duration</th>
                    <th>Provider</th>
                    <th>Created</th>
                  </tr>
                </thead>
                <tbody>
                  <For each={jobs()}>
                    {(job) => (
                      <tr>
                        <td>
                          <button
                            class="job-link"
                            onClick={() => void open(job)}
                          >
                            <span class="operation mono">
                              {job.operation.toUpperCase()}
                            </span>
                            <span class="job-title">
                              {job.first_line || "Waiting for a result"}
                            </span>
                            <small class="mono muted">
                              {job.id.slice(0, 8)}
                            </small>
                          </button>
                        </td>
                        <td>
                          <span class={`badge ${job.status}`}>
                            {job.status}
                          </span>
                        </td>
                        <td class="mono">{duration(job.duration_ms)}</td>
                        <td>
                          {job.provider ? providerNames[job.provider] : "—"}
                        </td>
                        <td class="nowrap muted">{date(job.created_at)}</td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <div class="table-footer">
              <span>
                {jobs().length} jobs loaded · Running jobs update automatically
              </span>
              <Show when={cursor()}>
                <button
                  class="button"
                  disabled={busy()}
                  onClick={() => void load(true)}
                >
                  Load more
                </button>
              </Show>
            </div>
          </Show>
        </Show>
      </section>
      <Show when={detail()}>
        {(job) => (
          <Modal title="Job details" close={() => setDetail()}>
            <div class="stack">
              <span class={`badge ${job().status}`}>{job().status}</span>
              <p class="job-first-line">
                {job().first_line || "This job is still processing."}
              </p>
              <dl class="details">
                <dt>Operation</dt>
                <dd>
                  {job().operation === "tts"
                    ? "Text to speech"
                    : "Speech to text"}
                </dd>
                <dt>Duration</dt>
                <dd>{duration(job().duration_ms)}</dd>
                <dt>Provider</dt>
                <dd>{job().provider ? providerNames[job().provider!] : "—"}</dd>
                <Show when={job().voice_profile}>
                  {(profile) => (
                    <>
                      <dt>Voice profile</dt>
                      <dd>
                        {profile().id} · revision {profile().revision}
                      </dd>
                    </>
                  )}
                </Show>
                <dt>Created</dt>
                <dd>{date(job().created_at)}</dd>
                <dt>Finished</dt>
                <dd>{date(job().finished_at)}</dd>
              </dl>
              <Show when={job().error_code}>
                <Notice error={`Job failed: ${job().error_code}`} />
              </Show>
              <label>
                Request ID
                <input readonly value={job().id} />
              </label>
              <Copy value={job().id} label="Copy request ID" />
              <p class="hint">
                History stores a summary. Full transcripts and generated file
                links appear in the original request’s result.
              </p>
            </div>
          </Modal>
        )}
      </Show>
    </>
  );
}
