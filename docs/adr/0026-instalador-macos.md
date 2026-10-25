# ADR 0026 — Instalador de macOS: `setup.sh` idempotente y reanudable

- Estado: aceptada (2026-10-25), Fase 5.4.

## Contexto

En el Mac había cuatro scripts que se ejecutaban a mano y en orden (`install.sh`, `install-edge.sh`, `install-widget.sh` y `make-signing-identity.sh`), más varios pasos que ningún script hacía:

- instalar y activar Karabiner-DriverKit-VirtualHIDDevice;
- emparejar con `--token` en la línea de comandos y Ctrl+C tras «connected»;
- dar Monitorización de entrada arrastrando el binario;
- dar Red local al widget.

macOS no deja hacer por script ni aprobar una extensión del sistema ni conceder un permiso de privacidad (TCC). A veces exige reiniciar a mitad.

## Decisión

- **Un `setup.sh` en POSIX sh**, que se ejecuta como el usuario y no con sudo. Pide sudo solo para las piezas del sistema; la contraseña va al Terminal.
  - Es **idempotente**: cada paso mira primero si ya está hecho.
  - Es **reanudable**: el paquete se copia al principio a `/usr/local/share/rawkvm`, que sobrevive a un reinicio, y se relanza desde allí (`sh /usr/local/share/rawkvm/setup.sh`).
  - Sale con 2 cuando macOS pide reiniciar, con 1 si algo falla y con 0 si todo queda bien.
- **Origen**, en el mismo orden que el de Windows (ADR 0024):
  1. `--from CARPETA`, con el `.tar.gz` y `SHA256SUMS` o con el paquete descomprimido;
  2. la carpeta del propio script;
  3. la release, con `curl` anónimo, o `gh` si falla.

  Para la última release sigue la redirección de `releases/latest`, sin parsear JSON: `python3` no viene con macOS. El tarball se comprueba contra `SHA256SUMS` y se le quita la cuarentena.
- **El paquete.** La release lleva `RawKVM-macos-<ver>.tar.gz`, solo para Apple silicon. Contiene `rawkvm-agent`, `rawkvm-edge`, `RawKVM.app` (firmada ad hoc), los scripts y plists de `deploy/macos`, `LEEME.md` y `COMMIT.txt`. Un Mac Intel compila (anexo de la guía): un universal duplicaría la build del widget por un caso que no tenemos.
- **Karabiner-DriverKit-VirtualHIDDevice fijado.**
  - **Versión:** 8.6.0, la probada.
  - **Comprobación:** el SHA-256 del `.pkg` va en el script; se comprobó contra el `digest` de GitHub. También se verifica la firma (`pkgutil --check-signature`, team `G43BCU2T37`, el mismo de la extensión activa). Después, `sudo installer`.
  - **Si ya está instalado** con otra versión, no se toca.
  - **Activación:** `Manager activate`, abrir el panel de Extensiones del controlador y esperar a `systemextensionsctl list` con `[activated enabled]` (máximo 5 minutos). Si el estado pide reiniciar, lo dice y sale.
  - **Con Karabiner-Elements** instalado, se usa lo suyo.
- **Emparejamiento sin Ctrl+C:**
  - **Agente.** `rawkvm-agent` gana `--pair` y `--token-file` (`agent_core::pairing`, como Linux y el gadget). `--token` queda oculto.
  - **Token.** `setup.sh` lo pide con `stty -echo` y lo deja en un fichero 0600 de su carpeta temporal, que borra al salir. El orden es: parar el servicio, `sudo rawkvm-agent --pair` y arrancarlo.
  - **Helper.** Toma el emparejamiento del agente (`--from-agent`, como antes).
  - **Widget.** Toma el del helper (`adopt_pairing`), ahora también en macOS. Se empareja una sola vez.
- **Permisos que no se pueden dar por script.** Abre el panel exacto de Ajustes, explica qué tocar y espera viendo el log:
  - **Monitorización de entrada** del agente (solo el LED). Además del panel, abre el Finder con el binario seleccionado (`open -R`) para arrastrarlo. Espera la línea `watching the keyboard LEDs` (máximo 3 minutos) y no es bloqueante; `--skip-led` se la salta. Si el log de `tccd` muestra el caso ad hoc → certificado (ADR 0016), ofrece `tccutil reset ListenEvent` avisando de que afecta a todas las apps.
  - **Red local** del widget. Si el widget registra `No route to host` mientras el helper sí llega al hub, es el permiso, y abre su panel.
  - **El helper de borde** no necesita ninguno, comprobado: solo lee y mueve el puntero con CoreGraphics.
- **Inicio de sesión del widget.** Se escribe el mismo LaunchAgent que el interruptor del widget (`tauri-plugin-autostart`, modo LaunchAgent: `~/Library/LaunchAgents/RawKVM.plist`, `Label` RawKVM y el ejecutable). Así el interruptor muestra el estado real.
- **`--check`**, de solo lectura:
  - una línea por pieza, en el formato de los otros sistemas (ADR 0023, ADR 0024);
  - la tabla «componente, instalado, permiso, servicio» del plan se queda en esas líneas, que se pegan en el chat igual que en Linux y Windows;
  - `rawkvm-agent --check` (con sudo, para leer `agent.toml`) da versión, configuración, hub, Karabiner y LED, del log de la ejecución actual (`agent_core::setup::current_run`);
  - `rawkvm-edge --check` da versión, configuración, `for_node` y hub;
  - el script añade la extensión, la identidad de firma, los servicios, el widget, su arranque y su conexión.
- **`uninstall.sh`.**
  - **Sin opciones:** quita el agente, el helper, el widget, su arranque, los plists y `/usr/local/share/rawkvm`.
  - **`--purge`:** también las configuraciones y los logs.
  - **`--karabiner`:** también el driver, con sus propios scripts. Nunca lo hace por defecto, y nunca toca Karabiner-Elements.
  - **Firma:** la identidad «RawKVM Local» se conserva.
- **CI.** Workflow `macos-tar` en macos-14:
  - construye y empaqueta;
  - prueba `setup.sh` en el runner: tarball corrupto rechazado; el paso de programas desde el tarball y desde su copia en `/usr/local/share/rawkvm`; `--check` en un Mac sin RawKVM que falla limpio; `uninstall.sh --purge`.

  El runner no puede aprobar una extensión del sistema, así que el resto se detiene con `RAWKVM_SETUP_ONLY=programs`, una variable solo para el test. `release.yml` adjunta el tarball.

## Consecuencias

- En un Mac nuevo: un comando, más tres o cuatro clics en Ajustes que macOS no deja automatizar, cada uno pedido por el script en su momento.
- Sin probar en el CI: la instalación de Karabiner y la aprobación de su extensión, la firma con el llavero, el emparejamiento real, los paneles de permisos y el diálogo de Red local. Se prueba en el Mac del usuario, desde cero (`uninstall.sh --purge` y `setup.sh`).
- `install.sh`, `install-edge.sh` e `install-widget.sh` siguen funcionando solos, para la instalación manual del anexo. `setup.sh` los llama.
