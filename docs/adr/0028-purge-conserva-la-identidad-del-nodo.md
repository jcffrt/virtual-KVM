# ADR 0028 — `--purge` conserva la identidad del nodo

- Estado: aceptada (2026-10-25), tras la prueba desde cero del instalador de macOS.

## Contexto

En la prueba desde cero en el Mac (`uninstall.sh --purge` y luego `setup.sh`), `--purge` borró las configuraciones enteras. Al emparejar de nuevo, el agente, el helper y el widget estrenaron `node_id`. El hub guarda tres cosas por `node_id`:

- en `nodes.toml`: el índice del nodo, y con él el atajo Ctrl, Ctrl, N;
- en `layout.toml`: los enlaces de borde y la posición en el lienzo del widget.

`--forget-node` libera el índice, pero no toca el layout. Los enlaces seguían apuntando al `node_id` viejo, así que el borde de GAMER hacia el Mac no hacía nada. El hub solo registraba «switch target is not a connected agent». Se arregló a mano editando `layout.toml` con el hub parado.

## Decisión

- **`uninstall.sh --purge` (macOS) conserva la identidad.** Borra el emparejamiento (dirección, token, certificado fijado, `for_node`) y los logs, pero deja `node_id` y `node_name` en cada configuración:
  - `/Library/Application Support/rawkvm/agent.toml`;
  - `edge.toml` y `widget.toml` del usuario.

  `setup.sh` vuelve a emparejar con el token y el Mac es los mismos nodos para el hub: mismo índice, mismo atajo, mismo sitio en el layout. Una configuración con solo la identidad se lee como «sin emparejar»; `setup.sh` y `--check` miran el token, no si existe el fichero.
- **`--purge --new-node`** lo borra todo, como antes, para cuando se quiera estrenar nodos de verdad.
- **El hub avisa de los nodos desconocidos en el layout.** Al cargar `layout.toml` (al arrancar y con SIGHUP), escribe un `warn` por cada `node_id` de los enlaces o del lienzo que no está en `nodes.toml`.
- **`rawkvm-hub --forget-node`** avisa si `layout.toml` todavía nombra ese nodo. No lo reescribe: el layout se vuelve a fijar en el widget.

## Consecuencias

- Reinstalar desde cero un Mac ya no rompe el borde ni cambia su índice.
- El hub registra la causa cuando un layout se queda con nodos viejos (por ejemplo tras un `--new-node`, o en una placa reinstalada), en lugar de solo «switch target is not a connected agent».
- **Sin hacer:**
  - que el widget dibuje como rotos los enlaces a nodos desconocidos;
  - lo mismo para `uninstall.ps1 -Purge` en Windows y `apt purge` del agente de Linux, que siguen borrando la identidad.

  Son candidatos si vuelve a pasar.
