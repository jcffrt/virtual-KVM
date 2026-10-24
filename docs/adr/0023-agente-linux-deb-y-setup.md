# ADR 0023 — Agente de Linux: paquete .deb y `rawkvm-agent setup`

- Estado: aceptada (2026-10-24), Fase 5.2.

## Contexto

En Ubuntu el agente se instalaba con `deploy/linux/install.sh`, que mezclaba pasos de root (la regla udev, `usermod -aG input`) con pasos del usuario: el binario en `~/.local/bin`, la unidad en `~/.config/systemd/user` y la extensión en `~/.local/share`. Después había que emparejar con `--token` en la línea de comandos y pulsar Ctrl+C tras «connected». En el Ubuntu de pruebas el orquestador no tiene `sudo`: la parte de root la hace el usuario y la del usuario puede hacerla el orquestador por SSH. Por eso la instalación tiene que separarse en dos pasos.

## Decisión

- **Paquete `rawkvm-agent_<ver>_amd64.deb`** con todo lo que es de root y nada de lo que es de un usuario:
  - `/usr/bin/rawkvm-agent`;
  - `/usr/lib/udev/rules.d/70-rawkvm-uhid.rules`;
  - `/usr/lib/modules-load.d/rawkvm-uhid.conf`;
  - la unidad **de usuario** `/usr/lib/systemd/user/rawkvm-agent.service` (`ExecStart=/usr/bin/rawkvm-agent`), sin activarla para nadie;
  - la extensión en `/usr/share/gnome-shell/extensions/rawkvm@olitec.cc/`.
- **Compilado con glibc 2.35** (`cargo zigbuild --target x86_64-unknown-linux-gnu.2.35`), para que valga desde Ubuntu 22.04. `Depends: $auto, systemd`: dpkg-shlibdeps calcula la versión de libc6 en el runner. El CI instala el paquete y ejecuta el binario en contenedores Ubuntu 22.04 y 24.04.
- **`/dev/uhid` con `uaccess`.** La regla añade `TAG+="uaccess"`, así logind da acceso al usuario de la sesión gráfica activa sin grupo `input` ni cerrar sesión. Se mantiene `GROUP="input"` para quien ya está en el grupo y para el uso sin sesión gráfica. Va en `70-` porque `uaccess` tiene que estar puesto antes de `73-seat-late.rules`; en el `99-` de antes no habría funcionado. `static_node=uhid`: el nodo existe antes de cargar el módulo.
- **`rawkvm-agent setup`**, como el usuario, nunca como root: lo rechaza.
  1. Retira la instalación manual del directorio personal: la unidad de `~/.config/systemd/user` (tapa la del paquete y arranca el binario viejo), `~/.local/bin/rawkvm-agent` y la copia de la extensión en `~/.local/share` (tapa la del paquete). Conserva `~/.config/rawkvm`.
  2. Empareja si hace falta. Ofrece los hubs que ve por mDNS o una dirección escrita, y pide el token sin eco o lo lee de `--token-file`. Antes para el servicio, que usaría el mismo id de nodo. Usa `pairing::pair` (ADR 0022).
  3. `systemctl --user enable` y `restart`.
  4. Activa la extensión: la añade a `enabled-extensions` con `gsettings`, porque eso sirve aunque el shell en marcha aún no la conozca, la quita de `disabled-extensions` y prueba `gnome-extensions enable`. Avisa de que en Wayland hay que volver a iniciar sesión una vez.
  5. Termina con `--check`.

  Se puede repetir: si ya está emparejado, no vuelve a emparejar salvo con `--pair`.
- **`rawkvm-agent --check`**: solo lee, y da una línea por pieza para pegar en el chat:
  - la versión;
  - `/dev/uhid` abierto en lectura y escritura;
  - la configuración, sin el token;
  - el estado de la unidad;
  - el último «connected», reconexión o error en el journal de este arranque;
  - si el nombre D-Bus de la extensión tiene dueño, y su estado según `gnome-extensions info` con `LC_ALL=C`, porque el Ubuntu está en castellano;
  - los restos de la instalación manual.

  Sale con 1 si algo es `FAIL`. Por SSH usa `$XDG_RUNTIME_DIR/bus` si falta `DBUS_SESSION_BUS_ADDRESS`.
- **`--pair`** también en el agente de Linux, igual que en el gadget.
- **postinst**: quita los ficheros de la instalación manual que estaban en `/etc` (la regla `99-rawkvm-uhid.rules` y `modules-load.d/rawkvm-uhid.conf`), carga `uhid` y vuelve a aplicar udev. Al actualizar, reinicia el agente de cada usuario que lo tenga en marcha (`systemctl --user -M usuario@ try-restart`). No toca los directorios personales.
- **`deploy/linux/setup-linux.sh`**: el comando único. Descarga según el ADR 0021 (`--from`, curl, gh), comprueba la línea del paquete en `SHA256SUMS` (no `--ignore-missing`, que aceptaría un fichero que no aparece en la lista), hace `sudo apt-get install` y lanza `setup` con `/dev/tty`, para que sea interactivo aunque venga de `curl | sh`.
- `deploy/linux/install.sh` queda como anexo con las mismas rutas que el paquete, y termina con `setup`.

## Consecuencias

- Root instala una vez para todos; cada usuario ejecuta `setup`. El orquestador puede hacer el segundo paso por SSH sin `sudo`.
- Con `uaccess`, el agente solo tiene `/dev/uhid` mientras el usuario tiene la sesión gráfica activa en el equipo, que es justo cuando sirve. Sin sesión gráfica, hace falta el grupo `input`.
- En Ubuntu 22.04 (GNOME 42) la extensión no carga: solo funciona en GNOME Shell 45 y posteriores, por ser un módulo ES. El agente sí funciona.
- Sin probar en el contenedor: `setup` completo (mDNS, systemd de usuario, gsettings), `uaccess` con una sesión real y el reinicio de los agentes al actualizar. Se valida en el Ubuntu real.
