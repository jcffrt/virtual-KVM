# Agente de Windows con driver VHF (aparcado)

> **Alternativa no recomendada (aparcada).** La vía recomendada para un PC Windows es el **agente USB gadget** ([gadget-instalacion.md](gadget-instalacion.md)): la placa se conecta por cable y Windows ve un receptor Logitech real, sin driver ni modo de prueba. Este driver queda en el repositorio sin seguimiento ni prueba en hardware; su CI solo corre cuando cambia su código (workflow `windows`, también manual).

El agente de Windows crea, con un driver propio (**RawKVM VHF**), una copia virtual exacta del receptor Logitech del hub: mismo descriptor, mismo VID/PID. Windows aplica su propio mapeo de teclas (AltGr, Win…) como si el receptor estuviera enchufado. Diseño: [ADR 0006](adr/0006-driver-windows-vhf.md).

Requisitos: Windows 10 2004 o posterior, o Windows 11, x64. Cuenta de administrador.

El driver va **firmado en modo de prueba** (test-signing): sirve para uso personal y requiere activar ese modo, lo que muestra una marca de agua "Modo de prueba" en el escritorio.

## 0. Descargar el paquete

En GitHub → repositorio `virtual-KVM` → **Actions** → último run verde de `ci` → artefacto **rawkvm-windows** (zip). Descomprimirlo, por ejemplo en `C:\rawkvm-windows`. Contiene:

```
rawkvm-agent.exe        agente (servicio)
driver\RawKvmVhf.sys    driver
driver\RawKvmVhf.inf
driver\RawKvmVhf.cat    catálogo firmado
driver\RawKvmTest.cer   certificado de prueba de esta build
install-driver.ps1, uninstall-driver.ps1, LEEME.md, COMMIT.txt
```

Abrir **PowerShell como administrador** en esa carpeta. Si PowerShell bloquea los scripts descargados:

```powershell
Unblock-File .\*.ps1
Set-ExecutionPolicy -Scope Process Bypass
```

## 1. Activar test-signing (una sola vez) y reiniciar

```powershell
.\install-driver.ps1 -EnableTestSigning
```

Si responde que `bcdedit` lo rechaza, el **Arranque seguro (Secure Boot)** está activo: hay que desactivarlo en la configuración UEFI del PC (al arrancar, F2/Supr/F10 según el fabricante → Security/Boot → Secure Boot = Disabled) y repetir. Con BitLocker activo, Windows pedirá la clave de recuperación tras el cambio: tenerla a mano (cuenta Microsoft → Dispositivos → Claves de recuperación).

**Reiniciar.** Tras el reinicio aparece la marca de agua "Modo de prueba" abajo a la derecha.

## 2. Instalar driver, agente y servicio

Con el token que muestra el hub (`sudo rawkvm-hub --show-token` en la Orange Pi):

```powershell
.\install-driver.ps1 -Hub 192.168.1.28 -Token <64 caracteres hex>
```

El script, en orden:

1. comprueba que test-signing está activo en el arranque actual;
2. importa `RawKvmTest.cer` en *Entidades de certificación raíz de confianza* y *Editores de confianza* del equipo (y borra los de builds anteriores);
3. copia el agente a `C:\Program Files\RawKVM\rawkvm-agent.exe`;
4. crea el dispositivo `Root\RawKvmVhf` e instala el driver (`rawkvm-agent driver-install`);
5. guarda hub y token en `C:\ProgramData\rawkvm\agent.toml` (solo SYSTEM y Administradores pueden leerlo);
6. crea e inicia el servicio **RawKvmAgent** (LocalSystem, automático, se reinicia si falla).

La primera vez Windows puede preguntar si confía en el editor "RawKVM": aceptar.

## 3. Comprobar

- **Log**: `Get-Content C:\ProgramData\rawkvm\agent.log -Wait -Tail 30`. Debe mostrar `opened \\.\RawKvmVhf`, `connected addr=192.168.1.28:4433 … index=N`, y `active` si es el destino actual.
- **Administrador de dispositivos** (`devmgmt.msc`) → *Dispositivos del sistema* → **RawKVM virtual HID bus** sin aviso amarillo. Con el agente conectado aparecen debajo (Ver → Dispositivos por conexión) tres "Dispositivo de entrada USB / HID-compliant device" y, colgando de ellos, **Teclado HID** (HID Keyboard Device), **Mouse compatible con HID** y los controles de consumidor/sistema.
- **Teclear**: con este PC como destino (si es el único agente conectado lo es; si no, **Ctrl, Ctrl, N** con su índice), escribir en el Bloc de notas con el MX Keys y mover el MX Ergo. Probar AltGr (`@`, `€`), la tecla Windows y las de volumen.
- **Teclas pegadas**: mantener la flecha derecha en el Bloc de notas y, en otra consola de administrador, `Restart-Service RawKvmAgent`: el cursor debe pararse.
- **Latencia**: en el log, `stats rtt_ms=… lost=…` cada 10 s.

## 4. Problemas conocidos

| Síntoma | Causa / solución |
|---|---|
| `install-driver.ps1` dice que test-signing está desactivado tras reiniciar | Secure Boot lo bloquea; ver paso 1. Comprobar con `bcdedit /enum {current}`. |
| Aviso amarillo en *RawKVM virtual HID bus*, código 52 | Firma no aceptada: test-signing inactivo o certificado no importado. Repetir `install-driver.ps1`. |
| Código 10 o 31 | Ver `C:\Windows\INF\setupapi.dev.log` (buscar RawKvmVhf) y enviar ese fragmento. |
| Log: `opening \\.\RawKvmVhf (is the RawKVM VHF driver installed?)` | El driver no está cargado: revisar el Administrador de dispositivos. |
| Log: `the hub rejected the pairing token` | Token incorrecto: `& 'C:\Program Files\RawKVM\rawkvm-agent.exe' configure --token <hex>` y `Restart-Service RawKvmAgent`. |
| Log: `the hub certificate is not the pinned one` | El hub se reinstaló: `… configure --unpin` y reiniciar el servicio. |

Para probar sin servicio (ver el log en la consola): `Stop-Service RawKvmAgent; & 'C:\Program Files\RawKVM\rawkvm-agent.exe' run`.

## 5. Desinstalar

```powershell
.\uninstall-driver.ps1          # -Purge borra también config y log
bcdedit /set testsigning off   # y reiniciar, si ya no se necesita
```
