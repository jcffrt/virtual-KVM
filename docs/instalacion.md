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

Instalador en preparación (Fase 5.3). Mientras tanto: helper de borde en [windows-borde.md](windows-borde.md) y widget en [widget-instalacion.md](widget-instalacion.md).

## macOS

Instalador en preparación (Fase 5.4). Mientras tanto: [macos-instalacion.md](macos-instalacion.md) y [widget-instalacion.md](widget-instalacion.md).
