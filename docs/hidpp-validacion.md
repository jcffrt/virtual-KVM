# Validar HID++ en la Pi (Fase 3 B)

Objetivo: saber qué feature de perfil de SO tiene el MX Keys (y el K230) y si al cambiarlo **en caliente** se cruzan Alt y Win/Cmd en el PC conectado. Con el resultado se decide el bloque C (ADR 0013).

Requisitos: el `rawkvm-hub` de este commit en la Pi, el **servicio parado** (el CLI abre `hidraw2` por su cuenta) y el usuario delante del Windows con el teclado conectado al receptor de la Pi. Ningún paso empareja ni desempareja nada.

```sh
sudo systemctl stop rawkvm-hub
sudo rawkvm-hub --hidpp-list --hidpp-trace | tee hidpp-list.txt
```

`--hidpp-list` muestra por ranura: nombre, tipo, PID inalámbrico, serie, versión HID++, y si tiene `HOSTS_INFO 1815` (hosts, host actual, SO del host actual, `can set OS`), `MULTIPLATFORM 4531` (plataformas y la actual) o `DUALPLATFORM 4530`. Si un dispositivo dice `not reachable`, pulsar una tecla o moverlo y repetir: puede estar dormido.

Cambio de perfil (ranura del MX Keys según la lista):

```sh
sudo rawkvm-hub --hidpp-set-os 3 windows --hidpp-trace | tee hidpp-set-windows.txt
# el usuario comprueba en el Windows: ¿Alt y Win están ya en su sitio?
sudo rawkvm-hub --hidpp-set-os 3 macos --hidpp-trace | tee hidpp-set-macos.txt
# y de vuelta: ¿se cruzan otra vez?
```

Imprime el estado antes, lo escrito y el estado después. Si `auto` no sirve, forzar la feature con `--hidpp-via multiplatform`, `--hidpp-via hostsinfo` (función 8 de 0x1815, sin verificar en hardware) o `--hidpp-via dualplatform`. Si no cambia en caliente, apagar y encender el teclado y comprobar de nuevo: así se sabe si solo se aplica al reconectar.

Al terminar:

```sh
sudo systemctl start rawkvm-hub
```

Sondeo crudo (solo si hace falta): `--hidpp-call <ranura> <feature hex> <función> [bytes hex]`, por ejemplo `--hidpp-call 3 1815 7 00` (getHostOsVersion del host 0). Ojo: una función desconocida puede cambiar el dispositivo.

Guardar los `.txt` con `--hidpp-trace` en `fixtures/hidpp/` (no contienen teclas: solo peticiones y respuestas HID++, y alguna notificación de conexión).
