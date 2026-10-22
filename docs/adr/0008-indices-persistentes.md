# ADR 0008 — Índices de agente y último activo persistentes

- Estado: aceptada (2026-10-22).

## Contexto

El atajo **Ctrl, Ctrl, N** elige el agente por su índice (1..4). Los índices vivían en memoria: al reiniciar el hub se repartían por orden de conexión, y la entrada iba al primero que conectaba. En la Orange Pi, tras un reinicio, el gadget pasó a ser el 1 (y activo) y el Mac el 2, al revés que antes.

## Decisión

- El hub guarda en `<state_dir>/nodes.toml` (por defecto `/var/lib/rawkvm/nodes.toml`, 0600, escritura atómica) el índice de cada `node_id` y el último agente activo. Se reescribe al asignar un índice nuevo y en cada cambio de agente activo.
- Un índice queda **reservado** para su nodo aunque esté desconectado. Un nodo nuevo recibe el primer índice libre y no reservado. Si todos los libres están reservados, se reasigna el primero libre con un aviso en el log en lugar de rechazar al agente.
- Al arrancar, durante **10 s** (`RESTORE_GRACE`) la entrada solo va al último activo cuando este conecta. Los demás se registran sin activarse. Si en ese plazo no aparece, al acabar se activa el agente conectado con el índice más bajo, o el primero que conecte después. El atajo y los cambios por borde funcionan durante la espera y la terminan.
- CLI: `rawkvm-hub --list-nodes` y `rawkvm-hub --forget-node <uuid|índice>`. Con el hub parado, porque el hub en marcha reescribe su copia.

## Consecuencias

- Un fallo al escribir el fichero solo se registra: se pierde la estabilidad de índices, no la sesión.
- Un PC retirado conserva su índice hasta `--forget-node`.
- Un último activo que no vuelve deja la entrada sin destino como mucho 10 s tras arrancar el hub.
