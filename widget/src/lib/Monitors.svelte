<script>
  import { invoke } from '@tauri-apps/api/core';
  import { untrack } from 'svelte';

  let { view } = $props();
  let cv = $state(null);
  let draft = $state(null); // placements being edited, null = the hub's
  let drag = $state(null); // { node, start: {x,y}, dx, dy }
  let message = $state('');
  let svg;

  const palette = ['#2563eb', '#16a34a', '#d97706', '#9333ea', '#0891b2', '#db2777'];
  const colour = (b) => (b.placeholder ? '#9ca3af' : palette[(b.index + palette.length - 1) % palette.length]);

  async function load() {
    cv = await invoke('canvas_view', { draft });
  }
  // Redraw when the hub state changes (screens, layout), unless dragging.
  $effect(() => {
    view;
    untrack(() => {
      if (!drag) load();
    });
  });

  const margin = $derived(cv ? Math.max(cv.bounds[2] - cv.bounds[0], cv.bounds[3] - cv.bounds[1]) * 0.06 : 0);
  const box = $derived(
    cv
      ? [cv.bounds[0] - margin, cv.bounds[1] - margin, cv.bounds[2] - cv.bounds[0] + 2 * margin, cv.bounds[3] - cv.bounds[1] + 2 * margin]
      : [0, 0, 1920, 1080],
  );
  const placementOf = (node) => cv.placements.find((p) => p.node === node);

  function toSvg(e) {
    const p = svg.createSVGPoint();
    p.x = e.clientX;
    p.y = e.clientY;
    return p.matrixTransform(svg.getScreenCTM().inverse());
  }

  function down(e, node) {
    e.currentTarget.setPointerCapture(e.pointerId);
    drag = { node, start: toSvg(e), dx: 0, dy: 0 };
  }
  function move(e) {
    if (!drag) return;
    const p = toSvg(e);
    drag = { ...drag, dx: p.x - drag.start.x, dy: p.y - drag.start.y };
  }
  async function up() {
    if (!drag) return;
    const d = drag;
    const p = placementOf(d.node);
    if (Math.abs(d.dx) < 1 && Math.abs(d.dy) < 1) {
      drag = null;
      return;
    }
    const next = await invoke('canvas_drop', { draft: cv.placements, node: d.node, x: p.x + d.dx, y: p.y + d.dy });
    cv = next;
    draft = next.placements;
    drag = null;
  }

  async function setScale(node, value) {
    const scale = value === '' || value == null ? null : Number(value);
    cv = await invoke('canvas_scale', { draft: cv.placements, node, scale });
    draft = cv.placements;
  }

  async function apply() {
    message = '';
    try {
      await invoke('canvas_apply', { draft: cv.placements });
      draft = null;
      message = 'Aplicado: los helpers usan ya la nueva disposición.';
    } catch (e) {
      message = String(e);
    }
  }
  async function discard() {
    draft = null;
    message = '';
    await load();
  }
  async function reset() {
    message = '';
    try {
      await invoke('canvas_reset');
      draft = null;
      message = 'El hub vuelve a la semilla de hub.toml.';
    } catch (e) {
      message = String(e);
    }
  }

  const offset = (node) => (drag && drag.node === node ? `translate(${drag.dx} ${drag.dy})` : '');
  const fontSize = $derived(box[2] / 70);
</script>

{#if !view.paired}
  <p class="muted">Empareja primero con un hub (pestaña Hub).</p>
{:else if cv}
  <p class="muted">
    Arrastra cada equipo. Donde los lados de dos equipos se tocan, el puntero pasa de uno a otro (línea azul). El paso
    entre las pantallas de un mismo equipo lo hace su sistema. Unidades lógicas (puntos).
    {#if !cv.custom}Ahora el hub usa la semilla de <span class="mono">hub.toml</span>; esta es una propuesta a partir de ella.{/if}
  </p>
  <svg
    bind:this={svg}
    viewBox={box.join(' ')}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    role="application"
    aria-label="Lienzo de monitores"
  >
    {#each cv.blocks as b (b.node)}
      <g transform={offset(b.node)} class="block" class:dragging={drag?.node === b.node}>
        {#each cv.rects.filter((r) => r.node === b.node) as r (r.screen)}
          <rect
            x={r.x}
            y={r.y}
            width={r.w}
            height={r.h}
            rx={fontSize * 0.6}
            fill={colour(b)}
            fill-opacity={r.placeholder ? 0.15 : 0.22}
            stroke={colour(b)}
            stroke-width={fontSize * 0.15}
            stroke-dasharray={r.placeholder ? `${fontSize} ${fontSize * 0.6}` : ''}
            onpointerdown={(e) => down(e, b.node)}
          />
          <text x={r.x + fontSize} y={r.y + fontSize * 1.8} font-size={fontSize * 1.1} class="name">
            {b.index ? `${b.index} · ` : ''}{b.name}
          </text>
          <text x={r.x + fontSize} y={r.y + fontSize * 3.2} font-size={fontSize * 0.85} class="detail">
            pantalla {r.screen} · {Math.round(r.w)}×{Math.round(r.h)} · {r.native}
          </text>
        {/each}
      </g>
    {/each}
    {#if !drag}
      {#each cv.links as l}
        <line x1={l.x1} y1={l.y1} x2={l.x2} y2={l.y2} class="link" stroke-width={fontSize * 0.5}>
          <title>{l.label}</title>
        </line>
      {/each}
    {/if}
  </svg>

  <div class="bar">
    <button class="primary" onclick={apply} disabled={!cv.dirty || !view.connected}>Aplicar</button>
    <button onclick={discard} disabled={!draft}>Descartar cambios</button>
    <button onclick={reset} disabled={!cv.custom || !view.connected}>Volver a la semilla del hub</button>
    <span class="muted">{cv.links.length} {cv.links.length === 1 ? 'enlace' : 'enlaces'}{cv.dirty ? ' · sin aplicar' : ''}</span>
  </div>
  {#if message}<p class="muted">{message}</p>{/if}

  <h3>Escala</h3>
  <p class="muted">
    Cada equipo se dibuja dividiendo sus medidas por la escala que informa su helper (macOS ya informa en puntos: 1). Si un
    bloque sale del tamaño equivocado, corrígela aquí; vacío = la informada.
  </p>
  <table>
    <thead><tr><th>Equipo</th><th>Informada</th><th>Corrección</th><th></th></tr></thead>
    <tbody>
      {#each cv.blocks as b (b.node)}
        <tr class:grey={b.placeholder}>
          <td>{b.index ? `${b.index} · ` : ''}{b.name}{#if b.placeholder}<span class="muted"> · sin helper</span>{/if}{#if !b.connected}<span class="muted"> · desconectado</span>{/if}</td>
          <td>{b.reported_scale != null ? `×${b.reported_scale}` : '—'}</td>
          <td>
            <input
              type="number"
              step="0.05"
              min="0.1"
              max="5"
              value={b.scale ?? ''}
              disabled={b.placeholder}
              onchange={(e) => setScale(b.node, e.currentTarget.value)}
            />
          </td>
          <td>{#if b.scale != null}<button onclick={() => setScale(b.node, null)}>Quitar</button>{/if}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{:else}
  <p class="muted">Cargando…</p>
{/if}

<style>
  svg { width: 100%; height: 340px; background: var(--panel); border: 1px solid var(--line); border-radius: 10px; touch-action: none; user-select: none; }
  .block rect { cursor: grab; }
  .block.dragging rect { cursor: grabbing; }
  .name { fill: var(--text); font-weight: 600; pointer-events: none; }
  .detail { fill: var(--muted); pointer-events: none; }
  .link { stroke: var(--accent); stroke-linecap: round; }
  .bar { display: flex; gap: 8px; align-items: center; margin: 12px 0; }
  h3 { margin: 18px 0 6px; font-size: 14px; }
  tr.grey td { color: var(--muted); }
  input[type='number'] { width: 80px; }
</style>
