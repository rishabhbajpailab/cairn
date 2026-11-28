<script lang="ts">
  import { api, flag } from '../api';
  import { subscribe } from '../live';
  import type { Item } from '../types';
  import ItemView from './ItemView.svelte';

  let { ongotocanvas }: { ongotocanvas?: () => void } = $props();

  const ONBOARDING = 'onboarding';
  const EXAMPLES = [
    { text: 'todo: ', label: 'todo:', hint: 'a to-do with a checkbox' },
    { text: 'htt: ', label: 'htt:', hint: 'hold that thought: park it for later' },
    { text: '', label: 'plain text', hint: 'lands in the inbox tagged #unsorted' },
  ];

  let items = $state<Item[]>([]);
  let steps = $state<Item[]>([]);
  let checklist = $state<Item | null>(null);
  let loaded = $state(false);
  let text = $state('');
  let error = $state('');
  let notice = $state('');
  let busy = $state(false);
  let input: HTMLInputElement;

  const rest = $derived(items.filter((i) => !i.tags.includes(ONBOARDING)));
  const done = $derived(steps.filter((s) => s.payload.done === true).length);

  async function refresh() {
    try {
      const [recent, stepItems, buckets] = await Promise.all([
        api.query({ updated_since: '-7d', limit: 200 }),
        api.query({ types: ['todo'], tags_any: [ONBOARDING], limit: 50 }),
        api.query({ types: ['bucket'], tags_any: [ONBOARDING], lifecycle: ['active', 'archived', 'trashed'], limit: 1 }),
      ]);
      items = recent;
      steps = stepItems.sort((a, b) => a.created_at.localeCompare(b.created_at));
      checklist = buckets[0] ?? null;
      error = '';
      if (!loaded) {
        loaded = true;
        await maybeSeed();
      }
    } catch (e) {
      error = (e as Error).message;
      loaded = true;
    }
  }

  /** First visit to a brand-new Cairn: seed the checklist once. The server is idempotent
   *  (it never re-seeds a checklist you archived or trashed); the flag just avoids asking. */
  async function maybeSeed() {
    if (items.length || steps.length || flag('onboarding.offered')) return;
    const anything = await api.query({ lifecycle: ['active', 'archived', 'trashed'], limit: 1 });
    if (anything.length) return;
    flag('onboarding.offered', true);
    await seed();
  }

  async function seed() {
    busy = true;
    try {
      const r = await api.onboard();
      notice = r.created ? 'Added a “Getting started” checklist to walk you through setup.' : 'The checklist already exists (it may be archived).';
      await refresh();
    } catch (e) {
      error = (e as Error).message;
    } finally {
      busy = false;
    }
  }

  async function archiveChecklist() {
    busy = true;
    try {
      await Promise.all([...steps, ...(checklist?.lifecycle === 'active' ? [checklist] : [])].map((i) => api.lifecycle(i.id, 'archived')));
      notice = 'Checklist archived. You’ll find it again under archived items.';
      await refresh();
    } catch (e) {
      error = (e as Error).message;
    } finally {
      busy = false;
    }
  }

  async function capture(e: SubmitEvent) {
    e.preventDefault();
    if (!text.trim()) return;
    try {
      const r = await api.capture(text);
      text = '';
      // The item appears when the spool consumer writes it; the hub event triggers refresh.
      notice = r.type ? `Captured as ${r.type}. It’ll appear in a moment.` : 'Captured. It’ll appear in a moment.';
      error = '';
    } catch (err) {
      error = (err as Error).message;
    }
  }

  function useExample(prefix: string) {
    text = prefix;
    input?.focus();
  }

  function replace(next: Item) {
    items = items.map((i) => (i.id === next.id ? next : i));
    steps = steps.map((i) => (i.id === next.id ? next : i));
  }

  $effect(() => {
    refresh();
    // Captures land asynchronously (spool consumer), so refresh when the hub says so.
    return subscribe(() => refresh());
  });
</script>

<form class="capture" onsubmit={capture}>
  <input
    bind:this={input}
    bind:value={text}
    placeholder="What’s on your mind? Try “todo: call the dentist” or “htt: idea for the garden”"
    aria-label="Capture"
  />
  <button type="submit" disabled={!text.trim()}>Capture</button>
</form>
<p class="hints">
  {#each EXAMPLES as ex}
    <button type="button" class="chip" onclick={() => useExample(ex.text)} title={ex.hint}>{ex.label}</button>
  {/each}
  <span class="muted">Reminders with a time (<code>remind:</code>) arrive with Discord in M4.</span>
</p>

{#if notice}<p class="notice" role="status">{notice} <button class="link" onclick={() => (notice = '')}>Dismiss</button></p>{/if}
{#if error}<p class="error" role="alert">{error}</p>{/if}

{#if !loaded}
  <ul class="list skeleton" aria-hidden="true">
    {#each [1, 2, 3] as _}<li><span class="bar"></span></li>{/each}
  </ul>
{:else}
  {#if steps.length}
    <section class="panel">
      <div class="panel-head">
        <h2>{checklist?.title ?? 'Getting started'}</h2>
        <span class="muted">{done} of {steps.length} done</span>
        <progress max={steps.length} value={done}></progress>
      </div>
      <ul class="list">
        {#each steps as step (step.id)}
          <li><ItemView item={step} kind="detail" onchange={replace} /></li>
        {/each}
      </ul>
      <div class="panel-foot">
        {#if done === steps.length}
          <span>All set. Nice work.</span>
          <button onclick={archiveChecklist} disabled={busy}>Archive the checklist</button>
        {:else}
          <button class="link" onclick={archiveChecklist} disabled={busy}>Hide this checklist</button>
        {/if}
      </div>
    </section>
  {/if}

  {#if rest.length}
    <h2 class="section">Last 7 days</h2>
    <ul class="list">
      {#each rest as item (item.id)}
        <li>
          <span class="badge">{item.type}</span>
          <ItemView {item} onchange={replace} />
          {#each item.tags as tag}<span class="tag">#{tag}</span>{/each}
        </li>
      {/each}
    </ul>
  {:else if !error}
    <div class="empty">
      <h2>{steps.length ? 'Nothing captured yet' : 'Your inbox is clear'}</h2>
      <p>Everything you capture from the last seven days shows up here, newest first. A few ways to add things:</p>
      <ul>
        <li><strong>Here:</strong> type in the box above. Prefixes pick the type; tap one of the chips to try it.</li>
        <li><strong>Your phone:</strong> an iOS Shortcut that POSTs to <code>/api/v1/capture</code> with a capture-only token.</li>
        <li><strong>Webhooks and Discord:</strong> configured in <code>config/cairn.toml</code>; see <code>docs/ingest.md</code>.</li>
      </ul>
      <p>
        Once you have a few items, open the <button class="link" onclick={() => ongotocanvas?.()}>Canvas</button> to arrange and link them.
      </p>
      {#if !steps.length && !checklist}
        <p><button onclick={seed} disabled={busy}>Add the “Getting started” checklist</button></p>
      {/if}
    </div>
  {/if}
{/if}
