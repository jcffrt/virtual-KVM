<script>
  import { invoke } from '@tauri-apps/api/core';

  let { view } = $props();
  let hysteresis = $state(150);
  let hotkey = $state(true);
  let push = $state(0);
  let corner = $state(3);
  let dead = $state(2);
  let autostart = $state(false);
  let message = $state('');
  let loadedSettings = false;
  let loadedOptions = false;

  // Take the hub's values once they arrive (and after each save).
  $effect(() => {
    if (view.settings && !loadedSettings) {
      hysteresis = view.settings.hysteresis_ms;
      hotkey = view.settings.hotkey;
      loadedSettings = true;
    }
  });
  $effect(() => {
    if (view.connected && !loadedOptions) {
      push = view.options.push_ms;
      corner = view.options.corner_pct;
      dead = view.options.dead_zone_px;
      loadedOptions = true;
    }
  });
  invoke('get_autostart').then((v) => (autostart = v)).catch(() => {});

  async function run(cmd, args, done) {
    message = '';
    try {
      await invoke(cmd, args);
      message = done;
    } catch (e) {
      message = String(e);
    }
  }
  const saveSettings = () =>
    run('set_settings', { hysteresisMs: Number(hysteresis), hotkey }, 'Guardado en el hub (settings.toml).');
  const saveOptions = () =>
    run(
      'set_edge_options',
      { pushMs: Number(push), cornerPct: Number(corner), deadZonePx: Number(dead) },
      'Guardado en el hub con la disposición; los helpers ya lo usan.',
    );
  const saveAutostart = () =>
    run('set_autostart', { enabled: autostart }, autostart ? 'Arrancará al iniciar sesión.' : 'Ya no arrancará solo.');
</script>

{#if !view.paired}
  <p class="muted">Empareja primero con un hub (pestaña Hub).</p>
{:else}
  <div class="card">
    <h3>Conmutación (hub)</h3>
    <label>
      Histéresis
      <input type="number" min="0" max="5000" step="10" bind:value={hysteresis} /> ms
      <span class="muted">tiempo mínimo entre dos cambios por borde</span>
    </label>
    <label>
      <input type="checkbox" bind:checked={hotkey} />
      Atajo de emergencia <span class="mono">Ctrl, Ctrl, 1…4</span>
    </label>
    <button class="primary" onclick={saveSettings} disabled={!view.connected}>Guardar</button>
  </div>

  <div class="card">
    <h3>Bordes (helpers)</h3>
    <label>
      Empujar
      <input type="number" min="0" max="2000" step="10" bind:value={push} /> ms
      <span class="muted">0 = cambio al tocar el borde; más, hay que mantenerlo</span>
    </label>
    <label>
      Esquinas
      <input type="number" min="0" max="40" bind:value={corner} /> %
      <span class="muted">de cada extremo del borde que no cambia de equipo</span>
    </label>
    <label>
      Zona muerta
      <input type="number" min="0" max="50" bind:value={dead} /> px
      <span class="muted">distancia al borde al entrar, antes de poder volver</span>
    </label>
    {#if !view.custom_layout}
      <p class="muted">El hub usa ahora la semilla de hub.toml: guardar esto guarda también la disposición de Monitores.</p>
    {/if}
    <button class="primary" onclick={saveOptions} disabled={!view.connected}>Guardar</button>
  </div>

  <div class="card">
    <h3>Teclas atascadas</h3>
    <p class="muted">Si una tecla o un botón se queda pulsado, el hub envía «nada pulsado» a todos los equipos.</p>
    <button onclick={() => run('release_keys', {}, 'Teclas liberadas.')} disabled={!view.connected}>Liberar teclas</button>
  </div>

  <div class="card">
    <h3>Este Mac</h3>
    <label>
      <input type="checkbox" bind:checked={autostart} onchange={saveAutostart} />
      Abrir RawKVM al iniciar sesión
    </label>
  </div>

  {#if message}<p class="muted">{message}</p>{/if}
{/if}

<style>
  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; margin-bottom: 14px; display: flex; flex-direction: column; gap: 8px; align-items: flex-start; }
  h3 { margin: 0 0 4px; font-size: 14px; }
  label { display: flex; gap: 8px; align-items: center; }
  input[type='number'] { width: 80px; }
</style>
