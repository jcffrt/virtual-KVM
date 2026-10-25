# ADR 0027 — Rastro de las muertes y relanzado del helper de Windows

- Estado: aceptada (2026-10-25), tras la validación de la Fase 5.

## Contexto

El 2026-10-25 a las 09:38, el helper de borde de GAMER (`rawkvm-edge.exe`, 99a1485) dejó de funcionar sin dejar rastro. Su log acaba con una línea «active» normal y Defender no tiene ninguna entrada. Hay dos problemas:

- **No hay rastro.** El helper es un proceso sin ventana (subsistema `windows`), así que no tiene stderr: el texto de un pánico de Rust no llega a ningún sitio. Tampoco dejan rastro un acceso indebido, un desbordamiento de pila o un `abort`.
- **Nadie lo vuelve a arrancar.** Se arranca al iniciar sesión desde `HKCU\…\Run`, una sola vez. Sin el helper no se puede salir de ese PC por el borde; solo funcionan Ctrl, Ctrl, N.

En los demás sistemas, el relanzado ya existe:
- **macOS:** los LaunchAgents/LaunchDaemons del agente y del helper llevan `KeepAlive`.
- **Ubuntu:** la unidad de usuario lleva `Restart=always`.
- **Gadget y hub:** sus unidades de systemd llevan `Restart=always`.

En todos ellos stderr ya acaba en el log (`StandardErrorPath` o journald). Con todo, una línea en el log propio, con fecha y hora, se encuentra mejor.

## Decisión

- **`agent_core::supervise::log_panics()`** instala un hook de pánico que escribe con `tracing` (nivel ERROR) el mensaje, el fichero:línea:columna y el hilo, y después llama al hook por defecto. Lo llaman, justo después de preparar el log:
  - los helpers de Windows y de macOS;
  - los agentes de macOS, Linux, Windows y el gadget.

  El hub no depende de `agent_core`; su stderr ya va a journald.
- **El helper de Windows se vigila a sí mismo.** Al arrancar en modo normal (desde Run, o sin opciones de línea de comandos), `rawkvm-edge.exe` es un supervisor:
  - lanza una copia de sí mismo con los mismos argumentos más `--supervised` (oculto), que es el helper de verdad;
  - espera a que termine. Si termina con éxito, termina él también. Si no, escribe en `edge.log` cuánto duró y cómo acabó, y la vuelve a lanzar.
  - **El código de salida** se escribe con nombre: 101 es un pánico (con su línea justo encima); también nombra el acceso indebido, el desbordamiento de pila, el *fail fast*/abort y TerminateProcess.
  - **Espera** 1 s antes de relanzar, y la duplica en cada muerte hasta 60 s. Vuelve a 1 s si la ejecución duró al menos 60 s.
  - **Un trabajo (job object)** con `KILL_ON_JOB_CLOSE`, en el que el supervisor se mete a sí mismo: el helper muere con él. Así nunca quedan dos helpers del mismo nodo, ni al volver a iniciar sesión ni al reinstalar con `setup.ps1`, que para los procesos por nombre.

  `--check` (y `setup.ps1 -Check`) lee el log desde el último arranque del supervisor. Si en esa ejecución hubo alguna muerte, añade `warn restarted` con la última.

Descartadas:
- **El widget relanza el helper.** El widget es opcional, y si muere él, nadie relanza a nadie.
- **Una tarea programada.** «Reiniciar si se produce un error» del Programador de tareas solo cubre los fallos al *iniciar* la tarea, no la salida del proceso. Además añadiría otro sitio que mantener en `setup.ps1` y `uninstall.ps1`.
- **Un servicio de Windows.** Pide administrador, y un servicio no puede leer el puntero de la sesión del usuario.

El supervisor es pequeño, no carga red ni ganchos y no lee la configuración, así que es poco probable que muera.

## Consecuencias

- En el Administrador de tareas aparecen dos `rawkvm-edge.exe` por usuario. Parar uno a mano no basta:
  - si se para el helper, vuelve en un segundo;
  - si se para el supervisor, se lleva el helper con él.
- Un error que haga terminar al helper (por ejemplo, una configuración inválida) se repite cada 60 s en el log en lugar de quedarse en una sola línea. Así se ve, y se arregla solo cuando se corrige la configuración.
- Prueba en el CI (`windows-zip`, `test-setup.ps1`):
  - matar el helper: vuelve y la muerte queda en `edge.log`;
  - `-Check` muestra `warn restarted`;
  - matar el supervisor: no queda ningún helper.
- Sin probar en hardware hasta que GAMER tenga el edge nuevo. Si vuelve a morir, `-Check` dirá cuándo y con qué código, y `edge.log` tendrá la línea del pánico si lo fue.
