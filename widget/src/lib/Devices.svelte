<script>
  let { view } = $props();
</script>

{#if !view.paired}
  <p class="muted">Empareja primero con un hub (pestaña Hub).</p>
{:else}
  <h3>Receptor</h3>
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

  <h3>Dispositivos emparejados</h3>
  <p class="muted">
    Listar los teclados y ratones emparejados con el receptor (nombre, batería) y emparejar uno nuevo necesita HID++ en el
    hub: llegará en la Fase 3, junto con el cambio de perfil de sistema del teclado al conmutar.
  </p>
  <button disabled>Emparejar nuevo dispositivo…</button>
{/if}

<style>
  h3 { margin: 18px 0 6px; font-size: 14px; }
  h3:first-child { margin-top: 0; }
</style>
