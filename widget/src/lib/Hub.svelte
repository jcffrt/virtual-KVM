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

  async function setExposure(receiver, value) {
    message = '';
    try {
      await invoke('set_exposure', { receiver, agent: value === 'network' ? null : value });
    } catch (e) {
      message = String(e);
    }
  }

  // The select goes back to what the hub says at once: if the hub refuses,
  // it answers with the same settings and Svelte would not reset a value
  // that did not change; if it accepts, the new value arrives with them.
  function changeExposure(select, e) {
    const wanted = select.value;
    select.value = e.agent ?? 'network';
    setExposure(e.receiver, wanted);
  }

  // Connected agents, plus the exclusive one of `e` if it is not connected.
  function exposureTargets(e) {
    const agents = view.nodes.filter((n) => n.role === 'agent').map((n) => ({ id: n.id, name: n.name }));
    if (e.agent && !agents.some((a) => a.id === e.agent)) {
      agents.push({ id: e.agent, name: `${e.agent_name ?? e.agent.slice(0, 8)} (desconectado)` });
    }
    return agents;
  }

  const roleLabel = { agent: 'agente', helper: 'helper de borde', widget: 'widget' };
</script>

{#if view.paired}
  <div class="card">
    <div class="row">
      <span class="dot" class:ok={view.connected} class:bad={!view.connected}></span>
      <strong>{view.hub_name ?? (view.hub_addr || 'Hub')}</strong>
      <span class="muted mono">{view.hub_addr || '(mDNS)'}</span>
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
          <td>
            {n.name}{#if n.active} <span class="badge">activo</span>{/if}
            {#if n.unexposed.length > 0}
              <div class="bad small" title="usb_f_hid admite cuatro interfaces HID por gadget (ADR 0031)">
                No llegan a su PC por USB: {n.unexposed.join('; ')}
              </div>
            {/if}
          </td>
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

  {#if view.exposures.length > 0}
    <h3 class="section">Receptores</h3>
    <table>
      <thead>
        <tr><th>N.º</th><th>Nombre</th><th>Interfaces</th><th>Entrada para</th></tr>
      </thead>
      <tbody>
        {#each view.exposures as e (e.receiver)}
          <tr class:grey={e.devices === 0}>
            <td>{e.receiver}</td>
            <td>{e.name}{#if e.devices === 0} <span class="muted">· sin conectar</span>{/if}</td>
            <td>{e.devices}</td>
            <td>
              <select
                value={e.agent ?? 'network'}
                disabled={!view.connected}
                onchange={(ev) => changeExposure(ev.currentTarget, e)}
              >
                <option value="network">Red local (todos los equipos)</option>
                {#each exposureTargets(e) as a (a.id)}
                  <option value={a.id}>Exclusivo → {a.name}</option>
                {/each}
              </select>
              {#if e.held}<span class="bad"> · su equipo no está conectado: la entrada se descarta</span>{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if view.exposures.some((e) => e.agent)}
      <p class="muted">
        Un receptor exclusivo no cambia de equipo con los bordes ni con el atajo.
        {#if view.hotkey}Para devolverlo a la red desde su teclado: {view.hotkey}, Esc.{:else}Para devolverlo a la red desde su teclado: el modificador del atajo dos veces y Esc.{/if}
      </p>
    {/if}
  {/if}
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
  .small { font-size: 12px; }
  tr.grey td { color: var(--muted); }
  tr.active td { font-weight: 600; }
  .badge { font-size: 11px; color: var(--ok); border: 1px solid var(--ok); border-radius: 4px; padding: 0 4px; font-weight: 500; }
  .hub { display: block; margin: 6px 0; }
  .form { display: flex; flex-direction: column; gap: 8px; margin-top: 12px; align-items: flex-start; }
  .form label { display: flex; gap: 8px; align-items: center; }
  h3 { margin: 0 0 8px; font-size: 14px; }
  h3.section { margin-top: 18px; }
</style>
