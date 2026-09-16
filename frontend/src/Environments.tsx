import { Heading } from "./ui";
export default function Environments(props: { signin: () => void; connect: () => void }) {
  return <>
    <Heading eyebrow="Sandbox" title="Testing environments" action={<button class="button primary" onClick={props.connect}>Use a test app_secret</button>}>
      Create and manage your shared testing environments in Honeycomb, then connect using Waveform’s app_secret.
    </Heading>
    <section class="panel settings-panel">
      <div class="panel-heading"><h2>Manage in Honeycomb</h2></div>
      <p>Honeycomb prepares, cleans, disables, restores and removes the shared environment. IAM supplies its test identities and permissions. Cleaning removes test history and preferences; restoring does not bring cleaned data back.</p>
      <p>After connecting, sign in with an IAM test SLT or an existing active test user’s public ID. Test speech uses prerecorded audio and stores it in the same Briefcase sandbox.</p>
      <div class="panel-footer"><a class="button" href="https://honeycomb.teamofsilicons.com" target="_blank" rel="noreferrer">Open Honeycomb</a><button class="button" onClick={props.connect}>Connect to a sandbox</button></div>
    </section>
  </>;
}
