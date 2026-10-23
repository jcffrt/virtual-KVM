# Instalación del agente en macOS

El agente de macOS convierte la entrada del receptor Logitech en un teclado y un ratón virtuales mediante **Karabiner-DriverKit-VirtualHIDDevice**. Corre como servicio del sistema (root), porque el socket de Karabiner solo admite root. Detalles técnicos: [ADR 0005](adr/0005-agente-macos-karabiner.md).

## 1. Karabiner-DriverKit-VirtualHIDDevice

1. Descargar el `.pkg` de la última versión de https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases (probado con 8.6.0) e instalarlo.
2. Activar la extensión del sistema:
   ```sh
   sudo '/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager' activate
   ```
3. Aprobarla en **Ajustes del Sistema → General → Ítems de inicio y extensiones → Extensiones del controlador** (o en el aviso que aparece) y, si lo pide, reiniciar.
4. Comprobar: `systemextensionsctl list | grep pqrs` debe mostrar `[activated enabled]`.

No hace falta Karabiner-Elements. Si ya está instalado, funciona igual (el instalador lo detecta y usa su daemon).

## 2. Compilar e instalar el agente

Una sola vez, crear la identidad de firma local (ver más abajo):

```sh
deploy/macos/make-signing-identity.sh      # sin sudo; pide sudo solo para confiar en el certificado
```

```sh
cargo build --release -p rawkvm-agent-macos
sudo deploy/macos/install.sh
```

Si se copia la carpeta a otro sitio, copiar `deploy/macos` **entera**: el instalador comprueba que tiene al lado los plists y `signing.sh` antes de parar nada, y si falta algo sale sin tocar los servicios.

**Firma estable** ([ADR 0016](adr/0016-firma-local-estable.md)). macOS liga los permisos (Monitorización de entrada del agente, Red local del widget) a la firma del programa. Con firma ad hoc cada versión es "otro programa" y los permisos dejan de valer al actualizar. `make-signing-identity.sh` crea en tu llavero de inicio de sesión un certificado autofirmado **RawKVM Local**, y `install.sh`, `install-edge.sh` e `install-widget.sh` firman con él si lo tienes (si no, ad hoc y lo avisan). Así los permisos se dan una vez y sobreviven a las versiones siguientes.

- La primera firma, macOS pregunta si `codesign` puede usar la clave «RawKVM Local»: **Permitir siempre**.
- Al pasar de firma ad hoc a «RawKVM Local» hay que dar los permisos **una última vez**. Red local del widget se renueva sola con aceptar el diálogo. **Monitorización de entrada del agente no**: macOS guarda en la entrada el requisito de la firma ad hoc y, como el agente es un daemon, no vuelve a preguntar. Queda denegado aunque el interruptor esté encendido y aunque se vuelva a arrastrar el binario (en el log de `tccd`: `Failed to match existing code requirement for subject /usr/local/bin/rawkvm-agent and service kTCCServiceListenEvent`). Una sola vez, `install.sh` lo avisa:
  1. `sudo tccutil reset ListenEvent`. Borra la Monitorización de entrada de **todas** las apps; habrá que volver a darla a las otras que la usen.
  2. Arrastrar de nuevo `/usr/local/bin/rawkvm-agent` a la lista y encender su interruptor.
  3. `sudo launchctl kickstart -k system/cc.olitec.rawkvm.agent`.
- Comprobar: `codesign -dr - /usr/local/bin/rawkvm-agent` debe mostrar `certificate leaf = H"…"`; con firma ad hoc muestra `cdhash`.
- Los artefactos de CI siguen firmados ad hoc; la firma estable se pone al instalar en el Mac.

El instalador:

- copia el binario a `/usr/local/bin/rawkvm-agent` y lo firma con «RawKVM Local» (o ad hoc si no existe);
- copia el emparejamiento de `~/Library/Application Support/rawkvm/agent.toml`, si existe, a `/Library/Application Support/rawkvm/agent.toml`;
- registra y arranca dos LaunchDaemons: `org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon` (el daemon de Karabiner, que no arranca solo) y `cc.olitec.rawkvm.agent`.

Si no había emparejamiento:

```sh
sudo launchctl bootout system /Library/LaunchDaemons/cc.olitec.rawkvm.agent.plist
sudo /usr/local/bin/rawkvm-agent --hub 192.168.1.28 --token <token del hub>   # Ctrl+C tras "connected"
sudo launchctl bootstrap system /Library/LaunchDaemons/cc.olitec.rawkvm.agent.plist
```

## 3. Comprobar

```sh
tail -f /var/log/rawkvm-agent.log
```

Debe aparecer: `connected to .../karabiner_virtual_hid_device_service.sock`, `Karabiner status kind=KeyboardReady value=true`, `PointingReady value=true`, `connected addr=192.168.1.28:4433`, `active`. Con el agente activo (único conectado, o con **Ctrl, Ctrl, número** de su índice), al teclear en el Logitech se escribe en el Mac y el trackball mueve el puntero.

La primera vez macOS puede abrir el **Asistente de configuración del teclado** para el teclado nuevo: pulsar las teclas que pide (o elegir ISO/ANSI) una vez.

Para el teclado y el ratón no hacen falta permisos de Monitorización de entrada ni Accesibilidad.

**LED de Bloq Mayús (Fase 3).** El daemon de Karabiner no avisa a sus clientes de los LED, pero su driver devuelve el estado que macOS fija en el teclado virtual como un informe de entrada. El agente lo lee abriendo ese teclado virtual (sin capturarlo, como hace Karabiner-Elements) y se lo manda al hub, que enciende el LED del teclado real. Para abrirlo, macOS pide **Monitorización de entrada** para el binario del agente:

1. Abrir la carpeta en el Finder con `open /usr/local/bin` y **arrastrar** `rawkvm-agent` a la lista de Ajustes del Sistema → Privacidad y seguridad → Monitorización de entrada, y activar su interruptor. (El botón **+** no deja entrar en `/usr/local/bin` desde su diálogo; ⌘⇧G y pegar la ruta funciona a veces.)
2. `sudo launchctl kickstart -k system/cc.olitec.rawkvm.agent` para que lo vuelva a intentar ya (si no, lo reintenta cada 5 s).

En el log debe aparecer `watching the keyboard LEDs of the Karabiner virtual keyboard`. Sin el permiso sale `keyboard LEDs not watched: not permitted` y todo lo demás funciona igual. Limitaciones: el estado se conoce desde el **primer cambio** después de abrir el teclado virtual (hasta entonces el hub deja el LED apagado al cambiar al Mac), y Karabiner solo devuelve Bloq Num y Bloq Mayús. Con firma ad hoc el permiso hay que darlo de nuevo tras cada reinstalación (quitar la entrada vieja y añadir la nueva); con «RawKVM Local» (paso 2) se conserva. El paso de ad hoc a «RawKVM Local» necesita reiniciar el permiso una vez (ver «Firma estable», arriba).

## 4. Diagnóstico sin Karabiner

```sh
cargo run -p rawkvm-agent-macos -- --dry-run
```

Conecta al hub con la configuración del usuario y registra los informes que enviaría a Karabiner (imprime las teclas; solo para pruebas).

## 5. Problemas conocidos

- `connecting to ... (is Karabiner-VirtualHIDDevice-Daemon running, and are we root?)`: el daemon de Karabiner no está en marcha (`sudo launchctl print system/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon`) o el agente no corre como root.
- `Karabiner driver extension not activated`: falta el paso 1.3.
- `Karabiner driver version mismatch`: el `.pkg` y la extensión activa son de versiones distintas; reinstalar el `.pkg` y repetir `activate`.
- `KeyboardReady` no llega a `true` tras actualizar Karabiner: cambió el protocolo de cliente (hoy 7); hay que actualizar `crates/agent-macos/src/karabiner.rs`.

## 6. Cambio de equipo por el borde (helper)

El agente es un servicio del sistema y no ve el puntero. El **helper de borde** (`rawkvm-edge`) corre en tu sesión, vigila el puntero unas 120 veces por segundo y, al tocar un borde enlazado en el layout del hub, pasa la entrada al otro equipo. Al volver a este Mac coloca el puntero en el borde de entrada; al salir lo aparca en el centro de la pantalla. **No pide permisos** (ni Monitorización de entrada ni Accesibilidad). Diseño: [ADR 0009](adr/0009-borde-helpers-y-layout.md).

Con el agente ya instalado y emparejado (paso 2):

```sh
cargo build --release -p rawkvm-edge-macos
sudo deploy/macos/install-edge.sh
```

El instalador:
- Comprueba que tiene al lado su plist y `signing.sh` antes de tocar nada.
- Copia el binario a `/usr/local/bin/rawkvm-edge` firmado con «RawKVM Local» (o ad hoc).
- Crea `~/Library/Application Support/rawkvm/edge.toml` (0600, tuyo) con el hub, el token y el certificado del agente, y su `node_id` como `for_node`.
- Instala el LaunchAgent `~/Library/LaunchAgents/cc.olitec.rawkvm.edge.plist`.

Log: `tail -f ~/Library/Logs/rawkvm-edge.log`. Debe mostrar `edge helper connected`, una línea `screen` por pantalla y `layout links=1`. Al llegar al borde: `edge reached`, y en el hub `switched`.

Diagnóstico sin hub:

```sh
rawkvm-edge --show
```

Lista las pantallas tal como las ve el helper (índice, origen y tamaño en puntos, escala) y luego la posición del puntero, la pantalla en la que está y el borde que toca.

- **Índices de pantalla**: la principal es la 0; las demás van de izquierda a derecha.
- **Layout**: lo decide el hub. Con `[[layout]] from = 2, edge = "right", to = 1`, el borde derecho de la pantalla más a la derecha lleva al Windows. Ver `rawkvm-hub --dump-layout` en la Pi.
- **Volver al Mac**: mientras no exista el helper de Windows, se vuelve con **Ctrl, Ctrl, 2**. Con el atajo el puntero no se mueve: aparece donde quedó aparcado.

## 7. Desinstalar

```sh
sudo deploy/macos/uninstall.sh
```

Quita el agente y, si lo ejecuta el mismo usuario, el helper de borde. Las configuraciones se conservan.
