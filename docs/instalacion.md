# Instalar RawKVM

RawKVM comparte un receptor de Logitech (teclado y ratón) entre varios equipos por la red. Hay tres piezas:

- el **hub**, en una placa Linux con el receptor enchufado;
- un **agente** en cada equipo que recibe la entrada (o una placa **gadget** conectada por USB al equipo);
- el **widget**, opcional, para cambiar de equipo con un clic y ver los dispositivos.

Las descargas son las de la [última release](https://github.com/jcffrt/virtual-KVM/releases/latest), sin cuenta de GitHub, con `SHA256SUMS` para comprobarlas ([ADR 0021](adr/0021-distribucion-por-releases.md)). Cada guía trae el comando de descarga.

El **token de emparejamiento** se escribe en cada equipo una vez. Se ve en el hub con `sudo rawkvm-hub --show-token`. No lo pases nunca en la línea de comandos (queda en el historial): los instaladores lo piden sin mostrarlo, o lo leen de un fichero con `--token-file`.

## Hub (Orange Pi, Raspberry Pi)

```sh
sudo apt install ./rawkvm-hub_<versión>_arm64.deb
sudo rawkvm-hub --show-token
```

Guía completa: [hub-linux-instalacion.md](hub-linux-instalacion.md).

## Placa gadget (PC por cable USB, sin software en el PC)

```sh
sudo apt install ./rawkvm-agent-gadget_<versión>_arm64.deb
sudo rawkvm-agent-gadget --pair --hub <ip-del-hub>
sudo systemctl enable --now rawkvm-agent-gadget
```

Guía completa: [gadget-instalacion.md](gadget-instalacion.md).

## Ubuntu

En la sesión del usuario (pide `sudo` solo para `apt install`):

```sh
curl -fsSL https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/linux/setup-linux.sh | sh
```

O en dos pasos: `sudo apt install ./rawkvm-agent_<versión>_amd64.deb` (administrador) y `rawkvm-agent setup` (el usuario). Comprobar: `rawkvm-agent --check`. Guía completa: [ubuntu-instalacion.md](ubuntu-instalacion.md).

## Windows

El PC Windows recibe el teclado y el ratón por el gadget USB (arriba). En él solo se instalan, por usuario y sin administrador, el helper de borde y el widget:

```powershell
irm https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/windows/setup.ps1 -OutFile $env:TEMP\rawkvm-setup.ps1
powershell -ExecutionPolicy Bypass -File $env:TEMP\rawkvm-setup.ps1
```

Pide la dirección del hub y el token (sin mostrarlo) y encuentra solo el gadget de ese PC. Guía: [windows-instalacion.md](windows-instalacion.md).

## macOS

En el Terminal, como tú (pide la contraseña solo cuando hace falta):

```sh
curl -fsSL https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/macos/setup.sh -o /tmp/rawkvm-setup.sh
sh /tmp/rawkvm-setup.sh
```

Instala Karabiner-DriverKit-VirtualHIDDevice, el agente, el helper de borde y el widget, y empareja una vez. En cada paso que macOS no deja hacer por script (aprobar la extensión, Monitorización de entrada, Red local) abre el panel de Ajustes y dice qué tocar. Se puede volver a ejecutar tras reiniciar o tras dar un permiso: `sh /usr/local/share/rawkvm/setup.sh`. Comprobar: `sh /usr/local/share/rawkvm/setup.sh --check`. Guía: [macos-instalacion.md](macos-instalacion.md).
