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

```sh
cargo build --release -p rawkvm-agent-macos
sudo deploy/macos/install.sh
```

El instalador:

- copia el binario a `/usr/local/bin/rawkvm-agent` y lo firma ad hoc;
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

No hacen falta permisos de Monitorización de entrada ni Accesibilidad para esta fase. Los necesitará el ayudante de detección de bordes de la Fase 2.

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
- Copia el binario a `/usr/local/bin/rawkvm-edge` con firma ad hoc.
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
