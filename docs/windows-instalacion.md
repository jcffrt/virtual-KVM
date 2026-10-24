# RawKVM en Windows: instalación

El PC Windows recibe el teclado y el ratón por el **gadget USB** de la placa ([gadget-instalacion.md](gadget-instalacion.md)): Windows ve un receptor Logitech real y no necesita nada de RawKVM para eso. En el PC se instalan dos programas **por usuario, sin administrador y sin driver**:

- **`rawkvm-edge.exe`**, el helper de borde: lee la posición del puntero unas 120 veces por segundo mientras el Windows tiene la entrada. Al tocar un borde enlazado en el layout del hub (por ejemplo el izquierdo, hacia el Mac) pide el cambio al hub, y cuando la entrada vuelve coloca el puntero en el borde de entrada a la altura correcta. No muestra ventana. Diseño: [ADR 0009](adr/0009-borde-helpers-y-layout.md).
- **`RawKVM.exe`**, el widget: icono en la bandeja para cambiar de equipo con un clic, colocar las pantallas y ver los dispositivos ([widget-instalacion.md](widget-instalacion.md)).

Decisiones del instalador: [ADR 0024](adr/0024-instalador-windows.md).

Requisitos: Windows 10 (actualizado) u 11, x64, con WebView2 (ya viene con ambos). Antes, el gadget instalado, emparejado y enchufado a este PC. Hacen falta la **dirección del hub** y su **token**, que se ve en el hub con `sudo rawkvm-hub --show-token`.

## Instalar

En una **PowerShell normal** (no hace falta abrirla como administrador):

```powershell
irm https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/windows/setup.ps1 -OutFile $env:TEMP\rawkvm-setup.ps1
powershell -ExecutionPolicy Bypass -File $env:TEMP\rawkvm-setup.ps1
```

O bien: descargar `RawKVM-windows-<versión>.zip` de la [última release](https://github.com/jcffrt/virtual-KVM/releases/latest), descomprimirlo y, dentro de la carpeta:

```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

`setup.ps1`:

1. **Programas**: sin zip al lado, descarga el de la última release y lo comprueba con `SHA256SUMS`. Los copia a `%LOCALAPPDATA%\Programs\RawKVM\` (con `setup.ps1` y `uninstall.ps1`) y les quita la marca de descargado de Internet. Si encuentra la instalación manual de antes (`%LOCALAPPDATA%\rawkvm\rawkvm-edge.exe`), la quita.
2. **Emparejamiento**, solo si no estaba hecho:
   - pide la **dirección del hub**, o Enter para buscarlo con mDNS;
   - pide el **token sin mostrarlo** (se puede pegar con clic derecho o Ctrl+V);
   - conecta una vez con el hub, fija su certificado y busca el gadget de este PC entre los equipos conectados. Si hay un solo agente de Windows en el hub, es ese. Si hay varios, descarta los que ya tienen el helper de otro PC, y si aún quedan varios, los lista y pregunta.

   El widget toma el mismo emparejamiento al arrancar: no hay que escribir el token en su ventana.
3. **Inicio de sesión**: los dos arrancan al iniciar sesión (clave `Run` del usuario: `RawKvmEdge` y `RawKVM`) y RawKVM aparece en el menú Inicio.
4. **Arranca** los dos y **comprueba** todo (lo mismo que `-Check`, abajo).

Repetirlo actualiza los programas sin tocar el emparejamiento.

Opciones:

| Opción | Para qué |
|---|---|
| `-Hub DIRECCIÓN` | No preguntar la dirección del hub. |
| `-TokenFile FICHERO` | Leer el token de un fichero en vez de escribirlo. |
| `-ForNode NODE_ID` | El gadget de este PC, si el hub tiene varios agentes de Windows (`sudo rawkvm-hub --list-nodes` en el hub). |
| `-Pair` | Emparejar otra vez, por ejemplo con un hub reinstalado. |
| `-From CARPETA` | Instalar desde una carpeta con el zip y `SHA256SUMS`, o con los programas ya descomprimidos. |
| `-Version v0.5.0` | Una release concreta en vez de la última. |
| `-NoStart` | No arrancar ahora; arrancan al iniciar sesión. |
| `-Check` | Solo comprobar (abajo). |

Sin consola (desde otro script) hay que pasar `-TokenFile`, y `-Hub` si hay más de un hub en la red.

### Avisos de Windows

- **ExecutionPolicy**: Windows no ejecuta scripts `.ps1` descargados con la política por defecto. `powershell -ExecutionPolicy Bypass -File …` la salta solo para ese script, sin cambiar nada del sistema.
- **SmartScreen**: los `.exe` no están firmados (un certificado de firma de código es de pago). Instalados con `setup.ps1` no hay aviso, porque les quita la marca de descargado de Internet (`Unblock-File`), y tampoco al iniciar sesión. Si se abre un `.exe` con doble clic desde el zip descargado, SmartScreen muestra «Windows protegió su PC»: **Más información → Ejecutar de todas formas**.
- **Firewall**: la conexión con el hub es saliente (QUIC, UDP 4433) y no necesita regla. Solo la búsqueda por mDNS recibe respuestas entrantes (UDP 5353). El firewall puede preguntar la primera vez, o descartarlas sin preguntar si el usuario no es administrador. Por eso `setup.ps1` guarda la dirección del hub y los programas no vuelven a usar mDNS. Si la búsqueda no encuentra el hub, escribir su IP.

## Comprobar

```powershell
powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\RawKVM\setup.ps1" -Check
```

No cambia nada. Una línea por pieza, para pegarla tal cual en el chat (el token no aparece nunca):

```
info  version    rawkvm-edge-windows 0.5.0 (1a2b3c4); widget 0.5.0; in C:\Users\javier\AppData\Local\Programs\RawKVM
ok    edge       C:\Users\javier\AppData\Roaming\rawkvm\edge.toml: hub 192.168.1.28, token set, certificate 3f2a9c1d7e0b...
ok    for node   6f1c2b9e-3d4a-4e8b-9c1d-2a7b5e6f8091
ok    hub        INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=0
ok    widget     C:\Users\javier\AppData\Local\rawkvm\widget.toml: hub 192.168.1.28, token set, certificate 3f2a9c1d7e0b...
ok    widget hub INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=0
ok    autostart  HKCU Run: RawKvmEdge, RawKVM
ok    processes  rawkvm-edge (4321), RawKVM (4322)
ok    firewall   hub address 192.168.1.28 saved: no mDNS, outbound QUIC only
```

Sale con 0 si nada es `FAIL`. Lo habitual:

| Línea | Qué hacer |
|---|---|
| `FAIL edge … missing` o `not paired` | `setup.ps1` (o `setup.ps1 -Pair`). |
| `FAIL for node … not set` | `setup.ps1 -Pair`; con el gadget enchufado y conectado al hub. |
| `FAIL hub … rejected the pairing token` | El token no es el del hub: `setup.ps1 -Pair`. |
| `FAIL hub … certificate is not the pinned one` | El hub se reinstaló: `setup.ps1 -Pair`. |
| `warn hub … reconnecting` | El hub no responde: ¿está en marcha y en la misma red? |
| `FAIL widget … not paired` | Abrir RawKVM una vez (toma el emparejamiento del helper al arrancar). |
| `FAIL autostart …` | `setup.ps1`. Si se desactivó en el Administrador de tareas (Inicio), `setup.ps1` lo vuelve a activar. |
| `FAIL processes … not running` | Cerrar sesión y volver a entrar, o `setup.ps1`. El log dice por qué se paró. |

Logs: `%LOCALAPPDATA%\rawkvm\edge.log` (helper) y `%LOCALAPPDATA%\rawkvm\rawkvm-widget.log` (widget). El del helper debe mostrar `connected`, una línea `screen` por monitor y `layout links=…`. En el hub, `journalctl -u rawkvm-hub` muestra `screens reported` del gadget.

Prueba: con el Windows activo, llevar el puntero al borde enlazado (por ejemplo el izquierdo), lejos de las esquinas. La entrada pasa al otro equipo y el puntero aparece en su borde. Volver por ese mismo borde.

## Problemas conocidos

- **No cambia en la pantalla de bloqueo, en UAC ni en Ctrl+Alt+Supr**: Windows no deja leer el puntero en el escritorio seguro. Usar **Ctrl, Ctrl, N**, que lo interpreta el hub.
- **`SetCursorPos failed`** en el log: lo mismo, o la ventana en primer plano se ejecuta como administrador. El cambio funciona igual, pero el puntero no se recoloca.
- **Escalado de pantalla (125 %, 150 %…)**: el helper trabaja en píxeles físicos (DPI por monitor), así que no le afecta.

Diagnóstico sin hub: lista los monitores (índice, origen y tamaño en píxeles, escala) y luego la posición del puntero, el monitor y el borde que toca. El principal es el monitor 0 y los demás van de izquierda a derecha.

```powershell
& "$env:LOCALAPPDATA\Programs\RawKVM\rawkvm-edge.exe" --show | Out-Host
```

## Desinstalar

```powershell
powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\RawKVM\uninstall.ps1"
```

Para los dos programas y quita la carpeta, el inicio de sesión y la entrada del menú Inicio. Conserva el emparejamiento (`%APPDATA%\rawkvm\edge.toml`, `%LOCALAPPDATA%\rawkvm\widget.toml`) y los logs. Con `-Purge` los borra también; el hub sigue listando los nodos de este PC hasta `sudo rawkvm-hub --forget-node` allí.

## Anexo: instalación manual

Para probar una build sin release. El zip del workflow `windows-zip` de GitHub Actions (artefacto `windows-zip`) es el mismo de la release. Desde el Mac:

```sh
gh run download -R jcffrt/virtual-KVM $(gh run list -R jcffrt/virtual-KVM -w windows-zip -b main -s success -L 1 --json databaseId -q '.[0].databaseId') -n windows-zip -D windows-zip
```

En el PC, descomprimir el zip y ejecutar `setup.ps1` dentro de la carpeta, como arriba. Sin `SHA256SUMS` al lado no hay nada que comprobar: instala los programas de esa carpeta.

Sin `setup.ps1`, a mano, en una PowerShell normal (el token, en un fichero, para que no quede en el historial):

```powershell
$dir = "$env:LOCALAPPDATA\Programs\RawKVM"
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item .\rawkvm-edge.exe, .\RawKVM.exe $dir; Get-ChildItem $dir | Unblock-File
& "$dir\rawkvm-edge.exe" --pair --hub 192.168.1.28 --token-file token.txt | Out-Host   # --for-node si hay varios gadgets
Remove-Item token.txt
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Set-ItemProperty $run RawKvmEdge "`"$dir\rawkvm-edge.exe`""
Set-ItemProperty $run RawKVM "`"$dir\RawKVM.exe`""
Start-Process "$dir\rawkvm-edge.exe"; Start-Process "$dir\RawKVM.exe"
& "$dir\rawkvm-edge.exe" --check | Out-Host
```

(`| Out-Host` hace que PowerShell espere al programa y muestre lo que escribe: es un programa sin ventana de consola propia.)

Compilar: el helper con `cargo build --release -p rawkvm-edge-windows` (MSVC) o desde Linux o el Mac con `--target x86_64-pc-windows-gnu`; el widget, en [widget-instalacion.md](widget-instalacion.md).

El driver VHF (un agente completo en Windows en vez del gadget) sigue aparcado: [windows-driver-vhf.md](windows-driver-vhf.md).
