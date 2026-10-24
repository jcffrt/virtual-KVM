# ADR 0024 — Instalador de Windows: `setup.ps1`, emparejamiento único y `-ForNode` automático

- Estado: aceptada (2026-10-24), Fase 5.3.

## Contexto

En el PC Windows, que recibe la entrada por el gadget USB, se instalaban dos programas por separado:

- el helper de borde, con `install-edge.ps1 -Hub … -Token … -ForNode …`. El token iba en la línea de comandos y el `node_id` del gadget había que copiarlo de `rawkvm-hub --list-nodes` en la Pi;
- el widget, un `.exe` suelto que se emparejaba a mano en su ventana con el mismo hub y el mismo token.

Además, `install.ps1` / `uninstall.ps1` eran los del driver VHF, aparcado (ADR 0006).

## Decisión

- **Por usuario, sin administrador.**
  - Los dos programas van en `%LOCALAPPDATA%\Programs\RawKVM\`, junto con `setup.ps1` y `uninstall.ps1` (para `-Check` y para desinstalar más tarde).
  - Les quita la marca de descargado de Internet (`Unblock-File`): SmartScreen no los para al iniciar sesión.
  - Arrancan desde la clave `Run` de HKCU:
    - `RawKvmEdge`;
    - `RawKVM`, el mismo nombre que usa el interruptor «Abrir RawKVM al iniciar sesión» del widget (tauri-plugin-autostart), así que el widget ve el estado real.
  - Borra la marca «Deshabilitado» del Administrador de tareas (`StartupApproved\Run`): instalar es activar.
  - Acceso directo en el menú Inicio.
  - Repetirlo actualiza los programas.
  - Retira la instalación manual anterior (`%LOCALAPPDATA%\rawkvm\rawkvm-edge.exe`; la clave `Run` se reescribe).
- **Origen y comprobación.** Por orden:
  1. `-From CARPETA`, con el zip y `SHA256SUMS`, o con los programas ya descomprimidos;
  2. la carpeta del propio script, si es el zip descomprimido;
  3. la release, con `Invoke-WebRequest` anónimo (`releases/latest`, o `-Version`), y `gh release download` si falla.

  El zip se comprueba contra su línea de `SHA256SUMS` antes de descomprimirlo (ADR 0021). La release lleva `RawKVM-windows-<ver>.zip`, con `rawkvm-edge.exe`, `RawKVM.exe`, `setup.ps1`, `uninstall.ps1`, `LEEME.md` y `COMMIT.txt`.
- **Un solo emparejamiento.**
  - `setup.ps1` empareja el helper con `rawkvm-edge --pair`.
  - El widget toma el emparejamiento del helper al arrancar (`agent_core::config::adopt_pairing`): hub, token y certificado de `%APPDATA%\rawkvm\edge.toml`, solo si están completos y solo si su `widget.toml` no tiene emparejamiento o es más antiguo que `edge.toml`.
  - Así, emparejar otra vez con `setup.ps1 -Pair` llega a los dos, y un emparejamiento hecho después en la ventana del widget se respeta.
  - Se descartó que el instalador escribiera las dos configuraciones: duplicaba el formato en PowerShell.
- **`-ForNode` deja de ser obligatorio, sin cambiar el protocolo.**
  - `rawkvm-edge --pair` conecta una vez como **widget** (`agent_core::pairing::survey`), con un `node_id` desechable para no tocar nada de lo que el hub guarda por nodo.
  - Lee el primer `Status`, fija el certificado y guarda la IP del hub si lo encontró por mDNS.
  - Elige el agente con `kind == Agent` y `os == Windows`. El gadget anuncia el sistema del PC (`--host-os windows`, el valor por defecto).
  - Si hay varios, descarta los que ya tienen el helper de otro PC. Si aún quedan varios, o ninguno, falla listándolos y `setup.ps1` pregunta cuál. `-ForNode` sigue valiendo para forzarlo.
  - `--pair` siempre olvida el certificado fijado: es para emparejar desde cero, por ejemplo con un hub reinstalado.
- **El token, nunca en la línea de comandos.**
  - `setup.ps1` lo pide con `Read-Host -AsSecureString`.
  - Lo deja en un fichero temporal de `%TEMP%`, dentro del perfil del usuario, se lo pasa al helper con `--token-file` y lo borra al terminar, también si falla.
  - O lo lee de `-TokenFile`.
  - `setup.ps1` no tiene parámetro `-Token`. Esto se aparta del plan, que decía «`-Hub` y `-Token`».
  - `--token` del helper sigue existiendo por compatibilidad, oculto en la ayuda.
- **El helper no lee de la consola.** Es un programa del subsistema GUI: con la consola compartida con PowerShell, leer de ella no es fiable. Toda la interacción (hub, token, elegir gadget) la hace `setup.ps1`, y el helper recibe argumentos.
- **`-Check`**, de solo lectura, con una línea por pieza y el mismo formato que `rawkvm-agent --check` (ADR 0023):
  - versión;
  - configuración y hub, de `rawkvm-edge --check`, que lee `edge.toml` y la última línea de conexión de `edge.log`;
  - `for_node`;
  - el emparejamiento y la conexión del widget;
  - las claves `Run`;
  - los procesos;
  - el cortafuegos.

  Usa texto ASCII (`...`, no `…`), porque Windows PowerShell 5.1 descodifica mal el UTF-8 de un programa por tubería.
- **Cortafuegos.**
  - La conexión con el hub es saliente (QUIC) y no necesita regla.
  - Las respuestas de mDNS son entrantes (UDP 5353), y un usuario sin administrador no puede permitirlas. Por eso `--pair` guarda la IP del hub, y los programas no vuelven a usar mDNS.
  - `-Check` avisa si `edge.toml` no tiene dirección.
- **`uninstall.ps1`.**
  - Sin opciones, quita los programas, las claves `Run` y el acceso directo, y conserva el emparejamiento y los logs.
  - `-Purge` los borra también.
  - Los scripts del driver VHF pasan a llamarse `install-driver.ps1` / `uninstall-driver.ps1`, y su guía a `windows-driver-vhf.md`.
- **CI.**
  - Workflow `windows-zip` (en `windows-latest`, con MSVC):
    - clippy y tests del helper y del widget;
    - el zip y su `SHA256SUMS`;
    - una prueba de instalación en el runner: zip corrupto rechazado; instalación sobre la manual anterior ya emparejada; ficheros, claves `Run`, acceso directo y procesos; `-Check`; emparejamiento forzado sin hub que falla limpio; `uninstall` y `-Purge`.
  - `release.yml` lo llama y adjunta el zip. El job `windows` de `widget.yml` pasa a este workflow.
  - PSScriptAnalyzer revisa todos los `.ps1` en `ci.yml`. Solo se excluye `PSAvoidUsingWriteHost`: son instaladores que hablan con la persona.
- **Código común.** `agent_core::setup` reúne lo que comparten los `setup` y `--check` de Linux y Windows: las líneas `ok/info/warn/FAIL`, la línea de configuración, el estado del hub según el log, la elección del hub y la petición del token. Le servirá también al de macOS.

## Consecuencias

- La instalación en el PC Windows es un comando, sin datos que copiar de la Pi salvo la IP y el token.
- Los `.exe` siguen sin firmar: abiertos con doble clic desde el zip descargado, SmartScreen avisa. Firmar con un certificado de pago queda fuera.
- Sin probar en el CI: emparejar contra un hub real (no hay hub en el runner de Windows; `survey` y la elección del gadget tienen tests en el workspace), la lectura oculta del token y la pregunta con varios gadgets.
