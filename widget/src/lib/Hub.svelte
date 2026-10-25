<script>
  import { invoke } from '@tauri-apps/api/core';

  let { view } = $props();
  let hubs = $state([]);
  let searching = $state(false);
  let addr = $state('');
  let token = $state('');
  let message = $state('');
  let confirmForget = $state(false);
  let confirmShutdown = $state(false);

  async function search() {
    searching = true;
    message = '';
    try {
      hubs = await invoke('discover');
      if (hubs.length === 0) message = 'No se encontró ningún hub en la red (mDNS). Escribe su dirección.';
    } catch (e) {
      message = String(e);
    }
    searching = false;
  }

  async function pair() {
    message = '';
    try {
      await invoke('pair', { hub: addr, token });
      token = '';
    } catch (e) {
      message = String(e);
    }
  }

  async function forget() {
    confirmForget = false;
    await invoke('forget');
  }

  async function shutdown() {
    confirmShutdown = false;
    message = '';
    try {
      await invoke('shutdown_hub');
    } catch (e) {
      message = String(e);
    }
  }

  async function switchTo(id) {
    try {
      await invoke('switch_to', { node: id });
    } catch (e) {
      message = String(e);
    }
  }

  const roleLabel = { agent: 'agente', helper: 'helper de borde', widget: 'widget' };
</script>

{#if view.paired}
  <div class="card">
    <div class="row">
      <span class="dot" class:ok={view.connected} class:bad={!view.connected}></span>
      <strong>{view.hub_name ?? view.hub_addr}</strong>
      <span class="muted mono">{view.hub_addr}</span>
      <span class="spacer"></span>
      {#if confirmForget}
        <span class="muted">Habrá que volver a emparejar con el token.</span>
        <button onclick={() => (confirmForget = false)}>Cancelar</button>
        <button class="primary" onclick={forget}>Olvidar</button>
      {:else}
        <button onclick={() => (confirmForget = true)}>Olvidar hub</button>
      {/if}
    </div>
    <dl>
      <dt>Estado</dt>
      <dd>
        {#if view.connected}Conectado{:else if view.fatal}<span class="bad">No se puede conectar</span>{:else if view.hub_off}Hub apagado{:else}Reconectando…{/if}
      </dd>
      <dt>Latencia</dt>
      <dd>{view.rtt_ms != null ? `${view.rtt_ms.toFixed(1)} ms` : '—'}</dd>
      <dt>Versión del hub</dt>
      <dd class="mono">{view.hub_version ?? '—'}</dd>
      <dt>Certificado</dt>
      <dd class="mono">{view.fingerprint ? `${view.fingerprint.slice(0, 16)}…` : 'se fijará al conectar'}</dd>
    </dl>
    {#if view.hub_off && !view.connected}
      <p class="muted">
        El hub se apagó ({view.hub_off}). Antes de desenchufar la placa, espera a que el LED verde deje de parpadear (el rojo es la alimentación y sigue encendido).
        Al volver a darle corriente, RawKVM se reconecta solo (lo intenta cada 30 s).
      </p>
    {:else if view.error && !view.connected}
      <p class:bad={view.fatal} class="muted">{view.error}</p>
    {/if}
    <div class="row actions">
      {#if confirmShutdown}
        <span class="muted">Todos los equipos se quedan sin teclado ni ratón hasta volver a encenderlo.</span>
        <button onclick={() => (confirmShutdown = false)}>Cancelar</button>
        <button class="primary" onclick={shutdown}>Apagar</button>
      {:else}
        <button onclick={() => (confirmShutdown = true)} disabled={!view.connected}>Apagar hub</button>
      {/if}
    </div>
  </div>

  <h3>Equipos</h3>
  <table>
    <thead>
      <tr><th>N.º</th><th>Nombre</th><th>Sistema</th><th>Tipo</th><th>Pantallas</th><th>RTT</th><th>Atajo</th><th></th></tr>
    </thead>
    <tbody>
      {#each view.nodes as n (n.id)}
        <tr class:grey={n.role === 'agent' && !n.has_helper} class:active={n.active}>
          <td>{n.role === 'agent' ? n.index : ''}</td>
          <td>{n.name}{#if n.active} <span class="badge">activo</span>{/if}</td>
          <td>{n.os}</td>
          <td>
            {roleLabel[n.role]}{#if n.helper_of} de {n.helper_of}{/if}
            {#if n.role === 'agent' && !n.has_helper}<span class="muted"> · sin helper</span>{/if}
          </td>
          <td>{n.role === 'agent' ? n.screens : ''}</td>
          <td>{n.rtt_ms != null ? `${n.rtt_ms.toFixed(0)} ms` : ''}</td>
          <td class="mono muted">{n.role === 'agent' && view.hotkey && n.index >= 1 && n.index <= 4 ? `${view.hotkey}, ${n.index}` : ''}</td>
          <td>
            {#if n.role === 'agent' && !n.active && view.connected}
              <button onclick={() => switchTo(n.id)}>Usar</button>
            {/if}
          </td>
        </tr>
      {:else}
        <tr><td colspan="8" class="muted">Sin datos del hub todavía.</td></tr>
      {/each}
    </tbody>
  </table>
{:else}
  <div class="card">
    <h3>Emparejar con un hub</h3>
    <p class="muted">
      El token se muestra en el hub con <span class="mono">sudo rawkvm-hub --show-token</span>.
    </p>
    <div class="row">
      <button onclick={search} disabled={searching}>{searching ? 'Buscando…' : 'Buscar hubs en la red'}</button>
    </div>
    {#each hubs as h}
      <label class="hub">
        <input type="radio" name="hub" value={h.addr} bind:group={addr} disabled={!h.compatible} />
        <strong>{h.name}</strong> <span class="mono muted">{h.addr}</span>
        {#if !h.compatible}<span class="bad"> · protocolo {h.proto_ver ?? '?'} (incompatible)</span>{/if}
      </label>
    {/each}
    <div class="form">
      <label>Dirección <input bind:value={addr} placeholder="192.168.1.28" /></label>
      <label>Token <input class="mono" bind:value={token} size="66" placeholder="64 caracteres hexadecimales" /></label>
      <button class="primary" onclick={pair} disabled={!addr || !token}>Emparejar</button>
    </div>
  </div>
{/if}

{#if message}<p class="bad">{message}</p>{/if}

<style>
  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; margin-bottom: 18px; }
  .row { display: flex; align-items: center; gap: 10px; }
  .spacer { flex: 1; }
  .actions { margin-top: 12px; }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: var(--muted); }
  .dot.ok { background: var(--ok); }
  .dot.bad { background: var(--bad); }
  dl { display: grid; grid-template-columns: 130px 1fr; gap: 4px 12px; margin: 12px 0 0; }
  dt { color: var(--muted); }
  dd { margin: 0; }
  .bad { color: var(--bad); }
  tr.grey td { color: var(--muted); }
  tr.active td { font-weight: 600; }
  .badge { font-size: 11px; color: var(--ok); border: 1px solid var(--ok); border-radius: 4px; padding: 0 4px; font-weight: 500; }
  .hub { display: block; margin: 6px 0; }
  .form { display: flex; flex-direction: column; gap: 8px; margin-top: 12px; align-items: flex-start; }
  .form label { display: flex; gap: 8px; align-items: center; }
  h3 { margin: 0 0 8px; font-size: 14px; }
</style>
