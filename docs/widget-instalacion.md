# Widget de RawKVM (Mac y Windows)

App de la barra de menús:
- muestra el hub y los equipos;
- cambia de equipo con un clic;
- empareja con el hub;
- coloca los monitores de todos los equipos (pestaña Monitores).

No mueve el teclado ni el ratón: eso lo hacen el agente y el helper de borde. Diseño: [ADR 0010](adr/0010-widget-y-protocolo-v3.md).

## Instalar

Del artefacto de CI **rawkvm-widget-macos** (trae `install-widget.sh`, `signing.sh` y `make-signing-identity.sh` junto a la app):

```sh
unzip RawKVM.app.zip
./make-signing-identity.sh                     # una sola vez por Mac; sin sudo
./install-widget.sh RawKVM.app                 # sin sudo
```

Desde el repositorio, lo mismo con `deploy/macos/install-widget.sh` (por defecto instala la app compilada, abajo).

El script quita la cuarentena, firma la app con la identidad local **RawKVM Local** si existe (`deploy/macos/make-signing-identity.sh`, ver [macos-instalacion.md](macos-instalacion.md) §2 y [ADR 0016](adr/0016-firma-local-estable.md)), cierra la que esté abierta, la copia a `~/Applications/RawKVM.app` y la abre. Si queda una copia antigua en `/Applications`, borrarla para que no haya dos.

**Red local**: la primera vez que la app busca o conecta con el hub, macOS pide permiso de **Red local**: aceptarlo (Ajustes del Sistema → Privacidad y seguridad → Red local). Firmada con «RawKVM Local», el permiso se conserva al instalar versiones nuevas. Firmada ad hoc, macOS la trata como otra app en cada versión: el interruptor sigue activo pero no vale, y la app no llega al hub (`No route to host`) hasta quitar la entrada y volver a darlo. Al pasar de ad hoc a «RawKVM Local» hace falta darlo una última vez. No pide más permisos.

Si se instala a mano (sin el script): mover `RawKVM.app` a `~/Applications`, `xattr -dr com.apple.quarantine ~/Applications/RawKVM.app`, y abrirla.

Compilar en el Mac (necesita Node 22 y Rust):

```sh
cd widget
npm ci
npx tauri build --bundles app      # src-tauri/target/release/bundle/macos/RawKVM.app
cd .. && deploy/macos/install-widget.sh
```

## Instalar en Windows

Con el instalador de Windows, que instala a la vez el widget y el helper de borde, los arranca al iniciar sesión y empareja los dos de una vez: [windows-instalacion.md](windows-instalacion.md). El widget toma el emparejamiento del helper (`%APPDATA%\rawkvm\edge.toml`), así que no hay que escribir el token en su ventana.

Notas:
1. Necesita **WebView2**, que ya viene con Windows 10 (actualizado) y 11.
2. No está firmado. Instalado con `setup.ps1` no hay aviso de **SmartScreen** (le quita la marca de descargado de Internet). Abierto con doble clic desde el zip descargado, SmartScreen avisa: **Más información → Ejecutar de todas formas**.
3. «Buscar hubs» usa mDNS. El **Firewall de Windows** puede preguntar si permite a RawKVM comunicarse en redes privadas: **Permitir**. Si se cancela, «Buscar hubs» no encuentra nada, pero escribir la IP del hub funciona.
4. No pide permisos de administrador ni instala drivers. El helper de borde (`rawkvm-edge`) es otro programa: el widget no mueve el puntero.

Compilar en Windows (Node 22 y Rust con MSVC):

```powershell
cd widget
npm ci
npx tauri build --no-bundle      # src-tauri\target\release\rawkvm-widget.exe
```

Diferencias con el Mac:
- **Bandeja** (área de notificación, junto al reloj): el icono es un cuadro azul con el número del equipo activo (en el Mac, el número va al lado del icono en la barra de menús). Sin equipo activo muestra los dos monitores. Puede quedar en el menú desplegable **^** de la bandeja: arrastrarlo a la barra para tenerlo siempre a la vista.
- **Clic izquierdo** en el icono abre la ventana; el **clic derecho** abre el menú (equipos, «Liberar teclas», «Ajustes…», «Salir»).
- **Opciones → Este PC**: «Abrir RawKVM al iniciar sesión» usa la clave `Run` del usuario (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`).
- Abrir el exe otra vez con RawKVM ya en marcha solo muestra su ventana (no arranca un segundo icono). Lo mismo en el Mac.

## Usar

- **Barra de menús**:
  - el icono de dos monitores lleva al lado el número del equipo activo;
  - el menú lista los equipos (1 Windows, 2 Mac…) con el activo marcado: elegir otro cambia la entrada al momento, sin pasar por el borde, y el puntero de ese equipo se queda donde estaba;
  - «Liberar teclas» hace que el hub mande «nada pulsado» a todos los equipos, por si una tecla se queda pulsada. También está en Opciones;
  - «Ajustes…» abre la ventana;
  - «Salir de RawKVM» cierra la app. Cerrar la ventana solo la oculta.
- **Pestaña Hub**:
  - **Sin emparejar**: «Buscar hubs en la red» (mDNS), elegir el hub o escribir su dirección, pegar el token (`sudo rawkvm-hub --show-token` en la Pi) y pulsar **Emparejar**. El certificado del hub se fija en la primera conexión, como en los agentes.
  - **Emparejado**: estado, latencia, versión del hub y lista de equipos: número, sistema, tipo, pantallas conocidas y su atajo (p. ej. `Ctrl, Ctrl, 2`). Un agente sin helper de borde sale **en gris** («sin helper»): se puede cambiar a él por el menú o con el atajo, pero no por el borde. «Usar» cambia a ese equipo. «Olvidar hub» borra el emparejamiento.

- **Pestaña Monitores**:
  - un bloque por equipo con sus pantallas reales (Mac: integrada y externa; Windows: sus dos monitores), en unidades lógicas;
  - un equipo sin helper de borde sale como bloque gris discontinuo de 1920×1080;
  - **arrastra** cada bloque: al soltarlo cerca de otro, se pega a su lado y alinea los bordes superiores;
  - las **líneas azules** marcan dónde el puntero pasará de un equipo a otro (solo en el tramo en que los lados se tocan);
  - **Aplicar** envía la disposición al hub, que la guarda (`/var/lib/rawkvm/layout.toml`) y la aplica al instante en los helpers, sin reiniciar nada;
  - **Descartar cambios** vuelve a lo que tiene el hub;
  - **Volver a la semilla del hub** borra la disposición guardada y el hub vuelve al `[[layout]]` de `hub.toml`;
  - **Escala**: cada equipo se dibuja dividiendo sus medidas por la escala que informa su helper (macOS ya informa en puntos, 1; Windows, su escalado de pantalla). Si un bloque sale de tamaño equivocado, se corrige a mano; vacío = la informada. La corrección se guarda en el hub con la disposición.
  - Mientras el hub use la semilla, la pestaña muestra una propuesta hecha a partir de ella. Hasta pulsar Aplicar no cambia nada.

- **Pestaña Dispositivos** (hub v4): los teclados y ratones emparejados con el receptor (ranura, nombre, tipo, si está conectado o dormido, y la disposición de teclas que tiene ahora: Windows o macOS). **Emparejar nuevo dispositivo…** abre el receptor 30 s con cuenta atrás: encender el dispositivo nuevo (o apagarlo y encenderlo) y esperar «Emparejado». **Olvidar** desempareja una ranura, con un segundo clic de confirmación.
  - **Batería** (hub v5): el nivel que da el dispositivo. Va por tramos: el MX Keys y el MX Ergo saltan, por ejemplo, de 50 % a 20 % y a 5 %, sin pasos intermedios. Con 10 % o menos sale en rojo con ⚠︎. Mientras carga, el MX Keys y el MX Ergo no dan nivel: sale solo "cargando". El hub la lee al conectarse el dispositivo, cada 5 minutos y cuando el dispositivo avisa de un cambio. Si está dormido, queda la última lectura.
  - **Ajustes de los dispositivos** (hub v5):
    - **Velocidad del puntero** de los ratones que la admiten (el MX Ergo): 1× es la normal. Se aplica al soltar el control; **1×** vuelve a la normal. Es un multiplicador, no los DPI del sensor: el MX Ergo cambia de DPI con su botón propio, y eso no se toca desde aquí.
    - **F1…F12 con función especial sin Fn** en los teclados que lo admiten (el MX Keys). Se aplica en el canal Easy-Switch actual del teclado.
    - Con el dispositivo dormido, los controles quedan desactivados.
  - Debajo, las interfaces HID que el hub reenvía.
- **Pestaña Opciones**:
  - **Conmutación (hub)**: histéresis (tiempo mínimo entre dos cambios por borde) y atajo de emergencia. El hub los aplica al instante y los guarda en `/var/lib/rawkvm/settings.toml`, que manda sobre `[switching]` de `hub.toml`.
    - **Atajo** (hub v6): dos pulsaciones cortas de la tecla elegida y luego un número del 1 al 4, que cambia a ese equipo. La tecla puede ser **Ctrl** (por defecto), **Mayús**, **Alt** o **Cmd/Win**; vale la de la izquierda o la de la derecha.
    - Con el atajo activado, la tecla elegida dos veces y después **0** vuelve al equipo anterior, y después **Esc** libera las teclas, como «Liberar teclas». Cada opción se puede desactivar.
    - Ni el número, ni el 0, ni el Esc llegan a ningún equipo.
    - Las dos pulsaciones sí llegan al equipo activo. Con Ctrl o Mayús no hacen nada, pero en Windows Win abre el menú Inicio y Alt marca el menú de la ventana.
    - La pestaña Hub (columna Atajo) y el menú de la barra muestran el atajo con la tecla elegida.
  - **Bordes (helpers)**: empujar (ms que hay que mantener el puntero en el borde; 0 = al tocarlo), esquinas (% de cada extremo que no cambia de equipo) y zona muerta (px). Viajan con la disposición: si el hub aún usa la semilla, guardar esto guarda también la disposición de Monitores.
  - **Este Mac**: abrir RawKVM al iniciar sesión (LaunchAgent del usuario).

## Ficheros

- Mac: `~/Library/Application Support/rawkvm/widget.toml` (hub, token y certificado fijado; el layout nunca, lo guarda el hub) y `~/Library/Logs/rawkvm-widget.log` (log).
- Windows: `%LOCALAPPDATA%\rawkvm\widget.toml` y `%LOCALAPPDATA%\rawkvm\rawkvm-widget.log`. Los guarda en el perfil local, no en el móvil (`%APPDATA%`): el certificado fijado es de este PC.

## Problemas

- **«No se encontró ningún hub»**: el mDNS no llega (otra red, o el router lo filtra). Escribir la IP del hub.
- **«No se puede conectar»** con un mensaje de token o certificado: «Olvidar hub» y volver a emparejar. Si el hub se reinstaló, su certificado cambió.
- **Un hub marcado «incompatible»**: el hub tiene otra versión de protocolo. Actualizar el hub o la app.
