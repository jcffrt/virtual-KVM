<script>
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import Hub from './lib/Hub.svelte';
  import Monitors from './lib/Monitors.svelte';

  const tabs = [
    { id: 'hub', label: 'Hub' },
    { id: 'devices', label: 'Dispositivos' },
    { id: 'monitors', label: 'Monitores' },
    { id: 'options', label: 'Opciones' },
  ];
  let tab = $state('hub');
  let view = $state(null);

  invoke('get_view').then((v) => (view = v));
  listen('state', (e) => (view = e.payload));
</script>

<main>
  <nav>
    {#each tabs as t}
      <button class:active={tab === t.id} onclick={() => (tab = t.id)}>{t.label}</button>
    {/each}
  </nav>
  <section>
    {#if !view}
      <p class="muted">Cargando…</p>
    {:else if tab === 'hub'}
      <Hub {view} />
    {:else if tab === 'monitors'}
      <Monitors {view} />
    {:else}
      <p class="muted">Próximamente.</p>
    {/if}
  </section>
</main>

<style>
  main { display: flex; flex-direction: column; height: 100vh; }
  nav { display: flex; gap: 4px; padding: 10px 14px; border-bottom: 1px solid var(--line); background: var(--panel); }
  nav button { border: none; background: transparent; }
  nav button.active { background: var(--bg); font-weight: 600; }
  section { padding: 16px 20px; overflow: auto; flex: 1; }
</style>
