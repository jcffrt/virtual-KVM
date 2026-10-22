# Cambio por el borde en el PC Windows (helper)

El PC Windows recibe el teclado y el ratón por el **gadget USB** de la Orange Pi ([gadget-instalacion.md](gadget-instalacion.md)) y no ejecuta nada de RawKVM para eso. Para cambiar de equipo llevando el puntero al borde de la pantalla hace falta este pequeño programa, `rawkvm-edge.exe`:

- Lee la posición del puntero unas 120 veces por segundo mientras el Windows tiene la entrada.
- Al tocar un borde enlazado en el layout del hub (por ejemplo el izquierdo, hacia el Mac), pide el cambio al hub.
- Cuando la entrada vuelve, coloca el puntero en el borde de entrada a la altura correcta. El resto del movimiento sigue llegando por el gadget.
- **Sin driver, sin permisos de administrador, sin servicio.** Arranca al iniciar sesión (clave `Run` del usuario) y no muestra ventana.

Diseño: [ADR 0009](adr/0009-borde-helpers-y-layout.md).

## 1. Datos que necesita

En la Orange Pi:

```sh
sudo rawkvm-hub --show-token      # token de emparejamiento
sudo rawkvm-hub --list-nodes      # el node_id del gadget (índice 1)
```

## 2. Instalar

Del artefacto de CI **rawkvm-edge-windows**: descomprimir y, en una PowerShell normal (no hace falta administrador), dentro de la carpeta:

```powershell
.\install-edge.ps1 -Hub 192.168.1.28 -Token <64 hex> -ForNode <node_id del gadget>
```

Si PowerShell no deja ejecutar scripts: `Set-ExecutionPolicy -Scope Process Bypass` en esa misma ventana.

El instalador:
- Copia el programa a `%LOCALAPPDATA%\rawkvm\rawkvm-edge.exe` y le quita la marca de descargado de Internet. El `.exe` no está firmado.
- Guarda la configuración en `%APPDATA%\rawkvm\edge.toml`.
- Lo añade al inicio de sesión y lo arranca.

Repetirlo actualiza el programa y conserva la configuración. Los parámetros solo hacen falta la primera vez.

## 3. Comprobar

Log: `%LOCALAPPDATA%\rawkvm\edge.log`. Debe mostrar:
- `edge helper connected`;
- una línea `screen` por monitor;
- `layout links=1`, con `[[layout]] from = 2, edge = "right", to = 1` en la Pi.

En la Pi, `journalctl -u rawkvm-hub` muestra `screens reported` del gadget.

Diagnóstico sin hub:

```powershell
& "$env:LOCALAPPDATA\rawkvm\rawkvm-edge.exe" --show | Out-Host
```

Lista los monitores (índice, origen y tamaño en píxeles, escala) y luego la posición del puntero, el monitor y el borde que toca. El principal es el monitor 0 y los demás van de izquierda a derecha.

Prueba: con el Windows activo, llevar el puntero al borde izquierdo, lejos de las esquinas. La entrada pasa al Mac y el puntero aparece en el borde derecho de su pantalla externa. Volver desde el Mac por ese borde.

## 4. Problemas conocidos

- **No cambia en la pantalla de bloqueo, en UAC ni en Ctrl+Alt+Supr**: Windows no deja leer el puntero en el escritorio seguro. Usar **Ctrl, Ctrl, N**, que lo interpreta el hub.
- **`SetCursorPos failed`**: lo mismo, o la ventana en primer plano se ejecuta como administrador. El cambio funciona igual, pero el puntero no se recoloca.
- **`no for_node`**: falta `-ForNode`.
- **`the hub rejected the pairing token`**: el token no es el de la Pi.
- **Tras reinstalar el hub** (certificado nuevo): `& "$env:LOCALAPPDATA\rawkvm\rawkvm-edge.exe" --unpin | Out-Host` y reiniciar sesión o volver a lanzar el programa.
- **Escalado de pantalla (125 %, 150 %…)**: el programa trabaja en píxeles físicos (DPI por monitor), así que no le afecta.

## 5. Desinstalar

```powershell
.\uninstall-edge.ps1
```

La configuración (`%APPDATA%\rawkvm\edge.toml`) se conserva.
