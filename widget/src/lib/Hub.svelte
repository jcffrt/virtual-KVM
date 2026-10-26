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
  // Receiver index waiting for the "Quitar" confirmation.
  let confirmRemove = $state(null);
  // A receiver was added or removed from this window: show the hub's answer.
  let receiverAsked = $state(false);

  // Bluetooth bridges (ADR 0033): the hub says the seconds of pairing left
  // when the bridge reported them; count down from there. Keyed by the
  // value, so a status resent for another reason does not restart it.
  let btDeadline = {};
  let now = $state(Date.now());
  $effect(() => {
    if (!view.nodes.some((n) => n.bridge?.discoverable_secs)) return;
    const t = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(t);
  });
  function btLeft(n) {
    const secs = n.bridge?.discoverable_secs;
    if (!secs) {
      delete btDeadline[n.id];
      return 0;
    }
    const d = btDeadline[n.id];
    if (!d || d.secs !== secs) btDeadline[n.id] = { secs, until: Date.now() + secs * 1000 };
    return Math.max(0, Math.ceil((btDeadline[n.id].until - now) / 1000));
  }
  let confirmBtForget = $state(null);
  async function bt(cmd, args) {
    message = '';
    confirmBtForget = null;
    try {
      await invoke(cmd, args);
    } catch (e) {
      message = String(e);
    }
  }

  async function addReceiver(u) {
    message = '';
    receiverAsked = true;
    try {
      await invoke('add_receiver', { vid: u.vid, pid: u.pid });
    } catch (e) {
      message = String(e);
    }
  }

  async function removeReceiver(receiver) {
    confirmRemove = null;
    message = '';
    receiverAsked = true;
    try {
      await invoke('remove_receiver', { receiver });
    } catch (e) {
      message = String(e);
    }
  }

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
    const agents = view.nodes
      .filter((n) => n.role === 'agent')
      .map((n) => ({ id: n.id, name: n.is_bridge ? `${n.name} (Bluetooth)` : n.name }));
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
        <tr class:grey={n.role === 'agent' && !n.has_helper && !n.is_bridge} class:active={n.active}>
          <td>{n.role === 'agent' && !n.is_bridge ? n.index : ''}</td>
          <td>
            {n.name}{#if n.active} <span class="badge">activo</span>{/if}
            {#if n.unexposed.length > 0}
              <div class="bad small" title="usb_f_hid admite cuatro interfaces HID por gadget (ADR 0031)">
                No llegan a su PC por USB: {n.unexposed.join('; ')}
              </div>
            {/if}
            {#if n.is_bridge}
              <div class="small">
                {#if n.bridge?.bonded}
                  PC: {n.bridge.bonded}
                  {#if n.bridge.connected}<span class="badge">conectado</span>{:else}<span class="muted"> · sin conexión</span>{/if}
                {:else}
                  <span class="muted">Sin PC emparejado</span>
                {/if}
              </div>
              {#if n.bridge?.repair_needed}
                <div class="bad small">
                  Han cambiado los dispositivos de este puente: en el PC, quita «{n.name}» de Bluetooth y vuelve a emparejarlo. Hasta entonces no llega la entrada.
                </div>
              {/if}
              <div class="row small">
                {#if btLeft(n) > 0}
                  <span>Visible para emparejar: {btLeft(n)} s</span>
                  <button onclick={() => bt('bt_discoverable', { node: n.id, secs: 0 })}>Parar</button>
                {:else}
                  <button onclick={() => bt('bt_discoverable', { node: n.id, secs: 60 })} disabled={!view.connected}>Hacer visible 60 s</button>
                {/if}
                {#if n.bridge?.bonded}
                  {#if confirmBtForget === n.id}
                    <button onclick={() => (confirmBtForget = null)}>Cancelar</button>
                    <button class="primary" onclick={() => bt('bt_forget', { node: n.id })}>Olvidar</button>
                  {:else}
                    <button onclick={() => (confirmBtForget = n.id)} disabled={!view.connected}>Olvidar PC</button>
                  {/if}
                {/if}
              </div>
            {/if}
          </td>
          <td>{n.os}</td>
          <td>
            {roleLabel[n.role]}{#if n.helper_of} de {n.helper_of}{/if}
            {#if n.is_bridge}<span class="muted"> · puente Bluetooth</span>{:else if n.role === 'agent' && !n.has_helper}<span class="muted"> · sin helper</span>{/if}
          </td>
          <td>{n.role === 'agent' && !n.is_bridge ? n.screens : ''}</td>
          <td>{n.rtt_ms != null ? `${n.rtt_ms.toFixed(0)} ms` : ''}</td>
          <td class="mono muted">{n.role === 'agent' && view.hotkey && n.index >= 1 && n.index <= 4 ? `${view.hotkey}, ${n.index}` : ''}</td>
          <td>
            {#if n.role === 'agent' && !n.is_bridge && !n.active && view.connected}
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
        <tr><th>N.º</th><th>Nombre</th><th>Interfaces</th><th>Entrada para</th><th></th></tr>
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
            <td class="nowrap">
              {#if confirmRemove === e.receiver}
                <button onclick={() => (confirmRemove = null)}>Cancelar</button>
                <button class="primary" onclick={() => removeReceiver(e.receiver)}>Quitar</button>
              {:else}
                <button
                  onclick={() => (confirmRemove = e.receiver)}
                  disabled={!view.connected || view.exposures.length < 2}
                  title={view.exposures.length < 2 ? 'El hub necesita al menos un receptor' : 'Deja de leerlo y lo borra de hub.toml'}
                >Quitar</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if confirmRemove !== null}
      <p class="muted">Sus dispositivos desaparecen de todos los equipos. Para volver a usarlo, aparecerá en «Sin configurar».</p>
    {/if}

    <h3 class="section">Sin configurar</h3>
    {#if view.unclaimed.length === 0}
      <p class="muted">Ningún receptor sin configurar.</p>
    {:else}
      <p class="muted">Dispositivos USB enchufados en el hub que no reenvía. Añadirlo los lleva a todos los equipos sin reiniciar el hub.</p>
      <table>
        <thead>
          <tr><th>Nombre</th><th>vid:pid</th><th>Interfaces</th><th>Contiene</th><th></th></tr>
        </thead>
        <tbody>
          {#each view.unclaimed as u (u.key)}
            <tr>
              <td>{u.name}</td>
              <td class="mono">{u.vid_pid}</td>
              <td>{u.interfaces}</td>
              <td>{u.kinds.join(', ')}</td>
              <td><button onclick={() => addReceiver(u)} disabled={!view.connected}>Añadir como receptor</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    {#if receiverAsked && view.receiver_result}
      <p class:bad={!view.receiver_result.ok} class:ok={view.receiver_result.ok}>
        {view.receiver_result.ok ? 'Hecho' : 'El hub no lo hizo'}: {view.receiver_result.message}
      </p>
    {/if}
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
  .ok { color: var(--ok); }
  .nowrap { white-space: nowrap; }
</style>
