<script>
  import { invoke } from '@tauri-apps/api/core';

  let { view } = $props();
  let message = $state('');
  // Slot waiting for the second click of "Olvidar" (WKWebView has no confirm()).
  let confirmSlot = $state(null);
  let now = $state(Date.now());

  // Tick the countdown while a pairing session is open.
  $effect(() => {
    if (!view.pairing.until_ms) return;
    const t = setInterval(() => (now = Date.now()), 500);
    return () => clearInterval(t);
  });
  let remaining = $derived(
    view.pairing.until_ms ? Math.max(0, Math.ceil((view.pairing.until_ms - now) / 1000)) : 0,
  );

  async function run(cmd, args) {
    message = '';
    try {
      await invoke(cmd, args);
    } catch (e) {
      message = String(e);
    }
  }

  async function unpair(slot) {
    confirmSlot = null;
    await run('unpair', { slot });
  }
</script>

{#if !view.paired}
  <p class="muted">Empareja primero con un hub (pestaña Hub).</p>
{:else}
  <h3>Dispositivos emparejados</h3>
  {#if view.receiver_devices == null}
    <p class="muted">
      El hub no ha leído el receptor por HID++ (hub anterior a la Fase 3, o la interfaz HID++ del receptor no está abierta).
    </p>
  {:else}
    <p class="muted">
      Teclados y ratones emparejados con el receptor del hub. El hub pone la disposición de teclas de cada teclado en la del
      sistema del equipo activo (Alt y Cmd/Win en su sitio).
    </p>
    <table>
      <thead><tr><th>Ranura</th><th>Nombre</th><th>Tipo</th><th>Estado</th><th>Disposición</th><th></th></tr></thead>
      <tbody>
        {#each view.receiver_devices as d (d.slot)}
          <tr>
            <td>{d.slot}</td>
            <td>{d.name} <span class="muted mono">{d.wpid}</span></td>
            <td>{d.kind}</td>
            <td>{#if d.online}<span class="ok">conectado</span>{:else}<span class="muted">dormido o apagado</span>{/if}</td>
            <td>
              {#if d.os_profiles}{d.profile ?? 'otra'}{:else}<span class="muted">—</span>{/if}
            </td>
            <td class="actions">
              {#if confirmSlot === d.slot}
                <span class="muted">Habrá que volver a emparejarlo.</span>
                <button onclick={() => (confirmSlot = null)}>Cancelar</button>
                <button class="primary" onclick={() => unpair(d.slot)}>Olvidar</button>
              {:else}
                <button onclick={() => (confirmSlot = d.slot)}>Olvidar</button>
              {/if}
            </td>
          </tr>
        {:else}
          <tr><td colspan="6" class="muted">Ningún dispositivo emparejado.</td></tr>
        {/each}
      </tbody>
    </table>

    <div class="pairing">
      {#if view.pairing.until_ms}
        <button onclick={() => run('pair_stop')}>Cancelar emparejamiento</button>
        <span>Emparejando… {remaining} s</span>
      {:else}
        <button class="primary" onclick={() => run('pair_start')} disabled={!view.connected}>
          Emparejar nuevo dispositivo…
        </button>
      {/if}
      {#if view.pairing.message}
        <span class:ok={view.pairing.ok} class:muted={!view.pairing.ok}>{view.pairing.message}</span>
      {/if}
      {#if message}<span class="bad">{message}</span>{/if}
    </div>
  {/if}

  <h3>Interfaces del receptor</h3>
  <p class="muted">Interfaces HID que el hub reenvía tal cual a cada equipo.</p>
  <table>
    <thead><tr><th>Id</th><th>Interfaz</th><th>Nombre</th><th>VID:PID</th><th>Contenido</th><th>Descriptor</th></tr></thead>
    <tbody>
      {#each view.devices as d (d.id)}
        <tr>
          <td>{d.id}</td>
          <td>{d.interface}</td>
          <td>{d.name}</td>
          <td class="mono">{d.vid_pid}</td>
          <td>{d.kinds.join(', ')}</td>
          <td class="muted">{d.descriptor_len} B</td>
        </tr>
      {:else}
        <tr><td colspan="6" class="muted">El hub no ha encontrado el receptor.</td></tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  h3 { margin: 18px 0 6px; font-size: 14px; }
  h3:first-child { margin-top: 0; }
  .actions { text-align: right; white-space: nowrap; }
  .actions button { margin-left: 4px; }
  .pairing { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; margin-top: 10px; }
  .ok { color: var(--ok); }
  .bad { color: var(--bad); }
</style>
