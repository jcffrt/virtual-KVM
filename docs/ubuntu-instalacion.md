# Agente en Ubuntu: instalación

El agente de Linux recrea con `/dev/uhid` los dispositivos del receptor que reenvía el hub: el sistema los ve como si el receptor estuviera enchufado en este equipo. Con GNOME, una extensión permite además cambiar de equipo por el borde de la pantalla ([gnome-plan.md](gnome-plan.md)). Decisiones de empaquetado: [ADR 0023](adr/0023-agente-linux-deb-y-setup.md).

Sistemas: Ubuntu 22.04 o posterior, en x86-64 (paquete `amd64`). La extensión necesita GNOME Shell 45 o posterior (Ubuntu 24.04 o más reciente); en 22.04 (GNOME 42) el agente funciona, pero sin cambio por el borde.

La instalación tiene **dos pasos**, y se pueden hacer en momentos distintos y por personas distintas:

1. **Como administrador** (`sudo`): instalar el paquete. Pone lo que necesita root: el programa, el acceso a `/dev/uhid`, el servicio y la extensión.
2. **Como el usuario que va a usar el equipo**, sin `sudo`: `rawkvm-agent setup`. Empareja con el hub y activa el servicio y la extensión para ese usuario.

## Con un solo comando

Desde la sesión del usuario, en un terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/jcffrt/virtual-KVM/main/deploy/linux/setup-linux.sh | sh
```

Descarga el paquete de la última release, lo comprueba con `SHA256SUMS`, pide la contraseña de `sudo` para `apt install` y lanza `rawkvm-agent setup`. Opciones: `--version v0.5.0`, o `--from CARPETA` si ya se tienen el `.deb` y `SHA256SUMS`. Lo que vaya detrás de `--` se pasa a `setup`.

## Paso a paso

### 1. Instalar el paquete (administrador)

```sh
v=$(curl -fsSL https://api.github.com/repos/jcffrt/virtual-KVM/releases/latest | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p')
base=https://github.com/jcffrt/virtual-KVM/releases/download/v$v
curl -fL -O "$base/rawkvm-agent_${v}_amd64.deb" -O "$base/SHA256SUMS"
sha256sum -c --ignore-missing SHA256SUMS
sudo apt install ./rawkvm-agent_${v}_amd64.deb
```

El paquete instala:

- `/usr/bin/rawkvm-agent`;
- la regla udev de `/dev/uhid`, que da acceso al usuario con la sesión gráfica abierta en el equipo (`uaccess`) y al grupo `input`, sin cerrar sesión;
- la carga de `uhid` en cada arranque;
- el servicio de usuario `rawkvm-agent.service` en `/usr/lib/systemd/user`, que no se activa para nadie hasta que el usuario ejecuta `setup`;
- la extensión de GNOME en `/usr/share/gnome-shell/extensions/rawkvm@olitec.cc`.

Al actualizar, reinicia el agente de los usuarios que lo tengan en marcha.

### 2. Emparejar y activar (el usuario)

```sh
rawkvm-agent setup
```

1. **Instalación anterior**: si existe la instalación manual de antes de la 0.5.0 (`~/.local/bin/rawkvm-agent`, la unidad en `~/.config/systemd/user` o la copia de la extensión en `~/.local/share/gnome-shell/extensions`), la quita. Conserva el emparejamiento (`~/.config/rawkvm/agent.toml`).
2. **Emparejamiento**: ofrece los hubs que encuentra en la red (mDNS), o deja escribir la dirección. Pide el token **sin mostrarlo** (se ve en el hub con `sudo rawkvm-hub --show-token`) y empareja. Si ya estaba emparejado, lo dice y sigue. `setup --pair` empareja de nuevo, por ejemplo con un hub reinstalado, y en ese caso Enter conserva el token guardado.
3. **Servicio**: `systemctl --user enable` y `restart` de `rawkvm-agent`.
4. **Extensión**: la activa. En Wayland, GNOME solo carga una extensión nueva al iniciar sesión: **hay que cerrar sesión y volver a entrar una vez**.
5. **Comprobación**: la misma que `rawkvm-agent --check`.

Opciones: `--hub DIRECCIÓN`, `--token-file FICHERO` (el token en un fichero en vez de escribirlo), `--pair` y `--no-extension`. Sin terminal (por ejemplo, desde un script), hay que pasar `--token-file`, y `--hub` si hay más de un hub en la red.

`setup` se puede repetir cuando se quiera: no rompe nada que ya funcione.

**Por SSH**: funciona si el usuario tiene además la sesión gráfica abierta en el equipo, porque el servicio y la extensión son de esa sesión. Sin sesión gráfica, el acceso a `/dev/uhid` solo llega por el grupo `input`.

## Comprobar

```sh
rawkvm-agent --check
```

No cambia nada. Una línea por pieza, para pegarla tal cual en el chat:

```
info  version    0.5.0 (40926a9) at /usr/bin/rawkvm-agent
ok    uhid       /dev/uhid read/write
ok    config     /home/javier/.config/rawkvm/agent.toml: hub 192.168.1.28, token set, certificate 3f2a9c1d7e0b...
ok    service    rawkvm-agent.service: enabled, active
ok    hub        INFO agent_core: connected addr=192.168.1.28:4433 hub=orangepi3 index=3
ok    extension  running; packaged; GNOME Shell 46, wayland
```

Sale con 0 si nada es `FAIL`. Lo habitual:

| Línea | Qué hacer |
|---|---|
| `FAIL uhid … Permission denied` | No hay sesión gráfica abierta en el equipo y el usuario no está en el grupo `input`. Abrir sesión en el equipo, o `sudo usermod -aG input $USER` y volver a entrar. |
| `FAIL config … missing` o `not paired` | `rawkvm-agent setup`. |
| `FAIL service … disabled` o `inactive` | `rawkvm-agent setup`, o `journalctl --user -u rawkvm-agent -e` para ver por qué se para. |
| `FAIL hub … rejected the pairing token` | El token no es el del hub: `rawkvm-agent setup --pair`. |
| `warn extension … not running` | Cerrar sesión y volver a entrar una vez. Si sigue, ver «Verificación» en [gnome-plan.md](gnome-plan.md). |
| `warn extension state OUT OF DATE` | La versión de GNOME Shell no está en la extensión: hace falta un paquete más nuevo. |

Registro del agente: `journalctl --user -u rawkvm-agent -f`.

## Desinstalar

- `sudo apt remove rawkvm-agent` para el agente de todos los usuarios y quita el programa. El emparejamiento de cada usuario (`~/.config/rawkvm`) se queda.
- Para borrar también el emparejamiento de un usuario: `rm -r ~/.config/rawkvm`.

## Anexo: instalación manual

Para probar un binario recién compilado sin hacer el paquete. Usa las mismas rutas que el paquete y termina con `setup`:

```sh
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.35 -p rawkvm-agent-linux   # en el Mac
scp target/x86_64-unknown-linux-gnu/release/rawkvm-agent <usuario>@<ubuntu>:
scp -r deploy/linux gnome-extension <usuario>@<ubuntu>:rawkvm/
ssh -t <usuario>@<ubuntu> 'sh rawkvm/linux/install.sh ./rawkvm-agent'
```

Para hacer el paquete en el Mac: después del `zigbuild`, `cargo deb --no-build --no-strip --target x86_64-unknown-linux-gnu -p rawkvm-agent-linux` (deja el `.deb` en `target/debian/`).
