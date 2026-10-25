<script>
  import { invoke } from '@tauri-apps/api/core';

  let { view } = $props();
  let hysteresis = $state(150);
  let hotkey = $state(true);
  let modifier = $state('ctrl');
  let previous = $state(true);
  let releaseKeys = $state(true);
  const modifierNames = { ctrl: 'Ctrl', shift: 'Mayús', alt: 'Alt', gui: 'Cmd/Win' };
  let tap = $derived(`${modifierNames[modifier]}, ${modifierNames[modifier]}`);
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
      hotkey = view.settings.hotkey.enabled;
      modifier = view.settings.hotkey.modifier;
      previous = view.settings.hotkey.previous;
      releaseKeys = view.settings.hotkey.release_keys;
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
    run(
      'set_settings',
      {
        hysteresisMs: Number(hysteresis),
        hotkey: { enabled: hotkey, modifier, previous, release_keys: releaseKeys },
      },
      'Guardado en el hub (settings.toml).',
    );
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
      Atajo de emergencia <span class="mono">{tap}, 1…4</span>
    </label>
    <label class:muted={!hotkey}>
      Tecla del atajo
      <select bind:value={modifier} disabled={!hotkey}>
        <option value="ctrl">Ctrl</option>
        <option value="shift">Mayús</option>
        <option value="alt">Alt</option>
        <option value="gui">Cmd / Win</option>
      </select>
      <span class="muted">dos pulsaciones cortas, sin nada más, y luego la tecla</span>
    </label>
    {#if hotkey && (modifier === 'alt' || modifier === 'gui')}
      <p class="muted">
        Cada pulsación llega también al equipo activo: en Windows, Win abre el menú Inicio y Alt marca el menú de la ventana.
      </p>
    {/if}
    <label class:muted={!hotkey}>
      <input type="checkbox" bind:checked={previous} disabled={!hotkey} />
      <span class="mono">{tap}, 0</span> vuelve al equipo anterior
    </label>
    <label class:muted={!hotkey}>
      <input type="checkbox" bind:checked={releaseKeys} disabled={!hotkey} />
      <span class="mono">{tap}, Esc</span> libera las teclas (como «Liberar teclas»)
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
    <h3>{view.platform === 'windows' ? 'Este PC' : 'Este Mac'}</h3>
    <label>
      <input type="checkbox" bind:checked={autostart} onchange={saveAutostart} />
      Abrir RawKVM al iniciar sesión
    </label>
    {#if view.platform === 'windows'}
      <label>
        <input
          type="checkbox"
          checked={view.power_off_with_pc}
          onchange={(e) =>
            run(
              'set_power_off_with_pc',
              { enabled: e.currentTarget.checked },
              e.currentTarget.checked
                ? 'El hub se apagará cuando este PC se apague o se reinicie.'
                : 'El hub ya no se apagará con este PC.',
            )}
        />
        Apagar el hub cuando este PC se apague
      </label>
      <p class="muted">
        Solo si la placa del hub se alimenta del USB de este PC. Al cerrar sesión o suspender no se apaga.
      </p>
    {/if}
  </div>

  {#if message}<p class="muted">{message}</p>{/if}
{/if}

<style>
  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px 16px; margin-bottom: 14px; display: flex; flex-direction: column; gap: 8px; align-items: flex-start; }
  h3 { margin: 0 0 4px; font-size: 14px; }
  label { display: flex; gap: 8px; align-items: center; }
  input[type='number'] { width: 80px; }
</style>
