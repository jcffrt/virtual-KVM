# RawKVM en macOS: instalación

En el Mac se instalan tres piezas:

- **el agente** (`rawkvm-agent`), un servicio del sistema (root) que convierte la entrada del receptor Logitech en un teclado y un ratón virtuales mediante **Karabiner-DriverKit-VirtualHIDDevice** ([ADR 0005](adr/0005-agente-macos-karabiner.md));
- **el helper de borde** (`rawkvm-edge`), en tu sesión: cambia de equipo al llevar el puntero a un borde enlazado ([ADR 0009](adr/0009-borde-helpers-y-layout.md));
- **el widget** (`RawKVM.app`), en la barra de menús ([widget-instalacion.md](widget-instalacion.md)).

Decisiones del instalador: [ADR 0026](adr/0026-instalador-macos.md).

Requisitos:

- macOS 13 o posterior, en un Mac con Apple silicon. En un Mac Intel hay que compilar: ver el anexo.
- La dirección del hub y su token, que se ve en el hub con `sudo rawkvm-hub --show-token`.

## Instalar

En el Terminal, **como tú, sin sudo**:

```sh
curl -fsSL https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/macos/setup.sh -o /tmp/rawkvm-setup.sh
sh /tmp/rawkvm-setup.sh
```

O bien: descargar `RawKVM-macos-<versión>.tar.gz` de la [última release](https://github.com/jcffrt/virtual-KVM/releases/latest), descomprimirlo y, dentro de la carpeta, ejecutar `sh setup.sh`.

La contraseña del Mac se pide **en el Terminal**, solo cuando hace falta (`sudo`).

Se puede **volver a ejecutar cuantas veces haga falta**: tras reiniciar, tras dar un permiso o para actualizar. Repite solo lo que falta y conserva el emparejamiento. El paquete queda en `/usr/local/share/rawkvm`, así que después basta con:

```sh
sh /usr/local/share/rawkvm/setup.sh
```

Lo que hace, por pasos (cada uno dice en qué está y qué falta):

1. **Programas**: sin el paquete al lado, descarga el de la última release y lo comprueba con `SHA256SUMS`. Lo copia a `/usr/local/share/rawkvm` y le quita la marca de cuarentena.
2. **Karabiner-DriverKit-VirtualHIDDevice**: si no está, descarga la versión fijada (8.6.0) y comprueba su SHA-256 y la firma de su autor. Después la instala (`sudo installer`) y activa la extensión del sistema.
   - **Hay que aprobarla a mano**: se abre Ajustes del Sistema → General → Ítems de inicio y extensiones → Extensiones del controlador. Activar «Karabiner-DriverKit-VirtualHIDDevice» (o «Permitir» en el aviso) y escribir la contraseña allí. El script espera hasta 5 minutos.
   - Si macOS pide **reiniciar**, el script lo dice y sale con código 2. Tras reiniciar, `sh /usr/local/share/rawkvm/setup.sh`.
   - Si **Karabiner-Elements** está instalado, usa su driver y su daemon y no instala nada de Karabiner.
3. **Identidad de firma «RawKVM Local»**: la crea una vez en tu llavero (pide sudo para confiar en el certificado). Con ella macOS conserva los permisos de una versión a la siguiente ([ADR 0016](adr/0016-firma-local-estable.md)). La primera firma pregunta si `codesign` puede usar la clave: **Permitir siempre**.
4. **Agente**: lo instala en `/usr/local/bin/rawkvm-agent`, firmado, como LaunchDaemon, con el daemon de Karabiner.
5. **Emparejamiento**, solo si no estaba hecho (o con `--pair`): pide la dirección del hub (Enter: buscarlo con mDNS) y el **token sin mostrarlo**. Conecta una vez, fija el certificado del hub y vuelve a arrancar el servicio.
6. **Monitorización de entrada** (solo para el LED de Bloq Mayús del teclado Logitech): un script no puede darla, así que abre el panel de Ajustes y el Finder con `rawkvm-agent` seleccionado.
   - Hay que **arrastrarlo** a la lista y encender su interruptor; el botón + no deja entrar en `/usr/local/bin`.
   - El script espera hasta 3 minutos. Si se pasa, sigue: todo lo demás funciona sin este permiso.
   - `--skip-led` se salta este paso.
   - Si una firma anterior del agente bloquea el permiso, ofrece `sudo tccutil reset ListenEvent`. Esto **borra la Monitorización de entrada de todas las apps**, así que pregunta antes.
7. **Helper de borde**: LaunchAgent de tu sesión. No pide permisos.
8. **Widget**: `~/Applications/RawKVM.app`, firmado, que arranca al iniciar sesión (el interruptor «Abrir RawKVM al iniciar sesión» sale activado).
   - Toma el emparejamiento del helper: no hay que escribir el token en su ventana.
   - La primera vez macOS pregunta si RawKVM puede **buscar dispositivos en la red local**: **Permitir**. Si se denegó, el script abre el panel (Privacidad y seguridad → Red local).
9. **Comprobación**: lo mismo que `--check`, abajo.

Opciones:

| Opción | Para qué |
|---|---|
| `--hub DIRECCIÓN` | No preguntar la dirección del hub. |
| `--token-file FICHERO` | Leer el token de un fichero en vez de escribirlo. |
| `--pair` | Emparejar otra vez, por ejemplo con un hub reinstalado. |
| `--skip-led` | No esperar al permiso de Monitorización de entrada. |
| `--from CARPETA` | Instalar desde una carpeta con el `.tar.gz` y `SHA256SUMS`, o con el paquete ya descomprimido. |
| `--version v0.5.0` | Una release concreta en vez de la última. |
| `--check` | Solo comprobar (abajo). |

Sin terminal (desde otro script) hay que pasar `--token-file`.

## Comprobar

```sh
sh /usr/local/share/rawkvm/setup.sh --check
```

No cambia nada; pide la contraseña para leer la configuración del agente, que solo lee root. Una línea por pieza, para pegarla tal cual en el chat (el token no aparece nunca):

```
info  macos           15.6 (arm64)
ok    karabiner       driver 8.6.0, extension activated and enabled
ok    signing         "RawKVM Local" in the login keychain
info  agent version   0.5.0 (1a2b3c4) at /usr/local/bin/rawkvm-agent
ok    agent config    /Library/Application Support/rawkvm/agent.toml: hub 192.168.1.28, token set, certificate 71a6f6577cf9...
ok    agent hub       ... INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepizero2w index=2
ok    agent karabiner ... Karabiner status kind=KeyboardReady value=true
ok    agent leds      ... watching the keyboard LEDs of the Karabiner virtual keyboard
ok    service         cc.olitec.rawkvm.agent running
ok    daemon          Karabiner VirtualHIDDevice daemon running
ok    edge config     /Users/javier/Library/Application Support/rawkvm/edge.toml: hub 192.168.1.28, token set, ...
ok    edge node       2c38b909-fd52-46fe-ac5c-300b57b71bb3
ok    edge hub        ... INFO agent_core: connected addr=192.168.1.28:4433 ...
ok    edge svc        cc.olitec.rawkvm.edge running
ok    widget          /Users/javier/Applications/RawKVM.app
ok    autostart       /Users/javier/Library/LaunchAgents/RawKVM.plist
ok    running         rawkvm-widget (4321)
ok    widget hub      agent_core::widget: widget connected addr=192.168.1.28:4433
```

Sale con 0 si nada es `FAIL`. Lo habitual:

| Línea | Qué hacer |
|---|---|
| `FAIL karabiner … approve it` | Aprobar la extensión en Ajustes (paso 2) y volver a ejecutar `setup.sh`. |
| `FAIL agent config … not paired` | `setup.sh --pair`. |
| `FAIL agent hub … rejected the pairing token` | El token no es el del hub: `setup.sh --pair`. |
| `FAIL agent hub … certificate is not the pinned one` | El hub se reinstaló: `setup.sh --pair`. |
| `warn agent hub … hub off` | El hub se apagó desde un widget; los equipos se reconectan solos al volver. |
| `FAIL agent karabiner … not activated` | Falta aprobar la extensión (paso 2). |
| `FAIL agent karabiner … Daemon running` | El daemon de Karabiner no está en marcha: `setup.sh`. |
| `warn agent leds … not watched` | Falta la Monitorización de entrada (paso 6); solo afecta al LED. |
| `FAIL widget hub … Local Network` | Ajustes → Privacidad y seguridad → Red local → RawKVM. |
| `warn agent … has no --check` | Un agente anterior a 0.5.0: `setup.sh` lo actualiza. |

Logs: `/var/log/rawkvm-agent.log` (agente), `~/Library/Logs/rawkvm-edge.log` (helper) y `~/Library/Logs/rawkvm-widget.log` (widget). Cada pieza tiene también su `--check`: `rawkvm-agent --check` (con sudo, para leer la configuración) y `rawkvm-edge --check`.

Prueba: con el Mac activo (único conectado, o con **Ctrl, Ctrl, número** de su índice), al teclear en el Logitech se escribe en el Mac y el trackball mueve el puntero. Al llevar el puntero al borde enlazado, la entrada pasa al otro equipo.

La primera vez macOS puede abrir el **Asistente de configuración del teclado** para el teclado nuevo: pulsar las teclas que pide (o elegir ISO/ANSI) una vez.

## Desinstalar

```sh
sh /usr/local/share/rawkvm/uninstall.sh
```

- **Sin opciones**: quita el agente, el helper, el widget y su arranque al iniciar sesión, y el paquete de `/usr/local/share/rawkvm`. Conserva el emparejamiento y los logs.
- **`--purge`**: borra también el emparejamiento y los logs, pero conserva el `node_id` y el nombre de cada pieza. Al volver a emparejar, el Mac es los mismos nodos para el hub: mismo índice del atajo y mismo sitio en el layout ([ADR 0028](adr/0028-purge-conserva-la-identidad-del-nodo.md)).
- **`--purge --new-node`**: lo borra todo, también la identidad. El hub sigue listando los nodos viejos hasta `sudo rawkvm-hub --forget-node` allí, y el layout hay que volver a fijarlo en el widget: el hub avisa en su log de los enlaces a nodos que ya no conoce.
- **`--karabiner`**: quita también Karabiner-DriverKit-VirtualHIDDevice, con sus propios scripts de desinstalación. **Nunca** lo hace por defecto, y nunca toca Karabiner-Elements.
- La identidad «RawKVM Local» se queda en el llavero; se borra con Acceso a Llaveros.

Probar desde cero: `sh /usr/local/share/rawkvm/uninstall.sh --purge` y luego el comando de instalación.

## Problemas conocidos

- `connecting to ... (is Karabiner-VirtualHIDDevice-Daemon running, and are we root?)` en el log del agente: el daemon de Karabiner no está en marcha (`sudo launchctl print system/org.pqrs.service.daemon.Karabiner-VirtualHIDDevice-Daemon`).
- `Karabiner driver version mismatch`: el `.pkg` y la extensión activa son de versiones distintas; reinstalar el `.pkg` y repetir `activate`.
- `KeyboardReady` no llega a `true` tras actualizar Karabiner: cambió el protocolo de cliente (hoy 7); hay que actualizar `crates/agent-macos/src/karabiner.rs`.
- **LED de Bloq Mayús**: el estado se conoce desde el **primer cambio** después de abrir el teclado virtual (hasta entonces el hub deja el LED apagado al cambiar al Mac), y Karabiner solo devuelve Bloq Num y Bloq Mayús.
- **Del helper**: con el atajo (**Ctrl, Ctrl, número**) el puntero no se mueve: aparece donde quedó aparcado. Al salir por un borde se aparca en el centro de la pantalla.

## Anexo: instalación manual

Para probar una build sin release, o en un Mac Intel. El paquete del workflow `macos-tar` de GitHub Actions (artefacto `macos-tar`) es el mismo de la release:

```sh
gh run download -R jcffrt/virtual-KVM $(gh run list -R jcffrt/virtual-KVM -w macos-tar -b main -s success -L 1 --json databaseId -q '.[0].databaseId') -n macos-tar -D macos-tar
sh setup.sh --from macos-tar      # desde la carpeta deploy/macos del repositorio, o con el setup.sh del paquete
```

Pieza a pieza, desde el repositorio:

1. Karabiner-DriverKit-VirtualHIDDevice:
   1. Instalar el `.pkg` de https://github.com/pqrs-org/Karabiner-DriverKit-VirtualHIDDevice/releases (8.6.0).
   2. Activarlo: `sudo '/Applications/.Karabiner-VirtualHIDDevice-Manager.app/Contents/MacOS/Karabiner-VirtualHIDDevice-Manager' activate`.
   3. Aprobarlo en Ajustes.
   4. Comprobar: `systemextensionsctl list | grep pqrs` debe decir `[activated enabled]`.
2. Identidad de firma, una vez: `deploy/macos/make-signing-identity.sh` (sin sudo).
3. Agente:
   ```sh
   cargo build --release -p rawkvm-agent-macos -p rawkvm-edge-macos
   sudo deploy/macos/install.sh target/release/rawkvm-agent-macos
   sudo launchctl bootout system/cc.olitec.rawkvm.agent
   sudo /usr/local/bin/rawkvm-agent --pair --hub 192.168.1.28        # pide el token sin mostrarlo
   sudo launchctl bootstrap system /Library/LaunchDaemons/cc.olitec.rawkvm.agent.plist
   ```
   Copiar `deploy/macos` **entera** si se lleva a otro sitio: los instaladores comprueban que tienen al lado los plists y `signing.sh` antes de parar nada.
4. Monitorización de entrada (LED):
   1. `open /usr/local/bin` y arrastrar `rawkvm-agent` a Ajustes → Privacidad y seguridad → Monitorización de entrada; encender su interruptor.
   2. `sudo launchctl kickstart -k system/cc.olitec.rawkvm.agent`.
   3. En el log debe aparecer `watching the keyboard LEDs`.

   **Al pasar de firma ad hoc a «RawKVM Local»**, la entrada vieja conserva el requisito de la firma anterior y macOS no vuelve a preguntar a un daemon. En el log de `tccd` sale `Failed to match existing code requirement`. Para arreglarlo, una vez:
   1. `sudo tccutil reset ListenEvent`. Afecta a **todas** las apps.
   2. Volver a arrastrar el binario.
   3. `sudo launchctl kickstart -k system/cc.olitec.rawkvm.agent`.
5. Helper: `sudo deploy/macos/install-edge.sh target/release/rawkvm-edge-macos`. Toma el hub, el token, el certificado y el `node_id` del agente. Diagnóstico sin hub: `rawkvm-edge --show`, que lista las pantallas y la posición del puntero.
6. Widget: [widget-instalacion.md](widget-instalacion.md).

Diagnóstico sin Karabiner: `cargo run -p rawkvm-agent-macos -- --dry-run` conecta al hub con la configuración del usuario y registra los informes que enviaría a Karabiner (imprime las teclas; solo para pruebas).
