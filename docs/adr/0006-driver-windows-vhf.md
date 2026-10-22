# ADR 0006 — Driver Windows: KMDF + Virtual HID Framework, test-signing por build

- Estado: aceptada (2026-10-22). Compilado en CI; pendiente de validar en los PCs.

## Contexto

Windows solo permite crear dispositivos HID desde modo kernel. El handoff fija un driver KMDF con **Virtual HID Framework (VHF)** que cree un HID virtual por cada `HidDevice` con el descriptor crudo, controlado desde el agente por IOCTL.

Referencias usadas (código y documentación de Microsoft, no de memoria): `vhf.h` (microsoft/wdkmetadata), el sample **HIDInjector** de microsoft/Windows-IoT-Samples (driver KMDF + VHF con interfaz para user mode), `Dmf_VirtualHidDeviceVhf.c` de microsoft/DMF, las páginas de `VhfCreate`, `VhfReadReportSubmit`, `VhfDelete`, `HID_XFER_PACKET`, `EVT_VHF_ASYNC_OPERATION` y "Write a HID source driver by using VHF", y el código de KMDF (microsoft/Windows-Driver-Frameworks) para `WdfDeviceCreateSymbolicLink`. El sample `vhidmini2` de Windows-driver-samples **no** usa VHF (es un minidriver HID UMDF/KMDF clásico), por eso la base es HIDInjector.

## Decisión

1. **Pila**: dispositivo raíz `Root\RawKvmVhf` (clase System), nuestro driver como función y `vhf` como `LowerFilters` (INF `.NT.HW`). VHF crea un PDO hijo por cada `VhfCreate` ("uno o más dispositivos" según la documentación) y hace de minidriver de transporte; encima cargan `hidclass` y `kbdhid`/`mouhid` como con un Logitech real.
2. **Interfaz**: enlace simbólico `\\.\RawKvmVhf` sobre el FDO (`WdfDeviceCreateSymbolicLink` usa el nombre del PDO). Seguridad en el INF: solo SYSTEM y Administradores. IOCTLs `METHOD_BUFFERED` (`drivers/windows-vhf/rawkvm_ioctl.h`, espejo en `crates/agent-windows/src/ioctl.rs` con tests de layout): `CREATE {cabecera 32 B + descriptor} → handle`, `REPORT {handle + informe}`, `DESTROY {handle}`, `DESTROY_ALL`. Cola por defecto a `PASSIVE_LEVEL` porque `VhfCreate`/`VhfDelete(Wait=TRUE)` lo exigen.
3. **Propiedad**: cada VHF pertenece al file object que lo creó; `EvtFileCleanup` borra los suyos. Si el agente muere, Windows cierra el handle y los dispositivos desaparecen (y con ellos cualquier tecla pulsada). El agente además envía "release all" antes de `DESTROY` en una parada ordenada (Stop del SCM).
4. **Report ID**: el buffer de `REPORT` es el informe tal como viaja por el cable (el que lee el hidraw del hub): con el byte de ID delante solo si el descriptor declara IDs. `HID_XFER_PACKET.reportId` = ese primer byte o 0 (el agente lo indica con `RAWKVM_FLAG_REPORT_IDS`). Es lo que entrega un minidriver de transporte; `hidclass` antepone el 0 a sus clientes cuando no hay IDs. Interfaz 0 (teclado boot, 8 B sin ID) y 1 (ratón con ID 2…) cubiertos por tests del agente.
5. **Informes del host**: `WriteReport` (LEDs, peticiones HID++ de software Logitech) se completa al momento con éxito y se descarta; `GetFeature`, `SetFeature` y `GetInputReport` no registran callback, así que VHF los completa con `STATUS_NOT_SUPPORTED`. Ninguna operación queda pendiente.
6. **Identidad**: VID/PID del `HidDevice`, `InstanceID = RAWKVM<id>`, y un `ContainerID` común derivado del `node_id` del agente (UUID v5) para que el Administrador de dispositivos agrupe las interfaces como un solo receptor. El descriptor se copia a pool no paginado y vive mientras exista el VHF.
7. **Creación del dispositivo raíz**: `pnputil` no crea dispositivos raíz y `[DeviceInstall32]` no está documentado en las páginas de secciones INF, así que el propio `rawkvm-agent.exe driver-install` hace lo que `devcon install`: crea `Root\RawKvmVhf` con SetupAPI si no existe y llama a `UpdateDriverForPlugAndPlayDevicesW`.
8. **Firma**: test-signing. `sign-test.ps1` firma el `.sys`, genera el catálogo con `inf2cat` (Windows 10 2004…11 24H2) y lo firma. En CI se crea **un certificado nuevo en cada build** (`New-SelfSignedCertificate`, solo EKU de firma de código); la clave privada muere con el runner, así que nadie puede firmar nada más con un certificado en el que los PCs confían. `install.ps1` importa el `.cer` de esa build en Raíz y Editores de confianza y retira los de builds anteriores. Alternativa descartada: un PFX persistente en un secreto de GitHub (no acumula certificados, pero deja viva una clave raíz de confianza).
9. **CI**: runner `windows-2022`, que trae el WDK 10.0.26100 completo (comprobado con un workflow de sondeo); `windows-2025` no lo trae. Sin instalación del WDK, coste en tiempo cero. KMDF 1.15, `DriverTargetPlatform=Desktop`, sin mitigaciones Spectre (las librerías no están en el runner), `SignMode=Off` en el proyecto.

## Consecuencias

- **Test-signing** muestra la marca de agua "Modo de prueba" y **exige desactivar Secure Boot** (bcdedit rechaza `testsigning on` con Secure Boot activo). Con BitLocker, cambiar Secure Boot o el BCD pide la clave de recuperación. Para producción haría falta firma de atestación de Microsoft (cuenta de Partner Center con certificado EV) — fuera de alcance.
- Requiere Windows 10 2004+ (`ExAllocatePool2`, modelos `10.0...19041` del INF).
- Las peticiones del host (LED de Bloq Mayús, HID++) no llegan al receptor; el software Logitech (Options+) no verá el dispositivo como configurable.
- Sin detección de bordes todavía (Fase 2): el servicio corre en la sesión 0 y la detección necesitará un helper en la sesión del usuario (`WH_MOUSE_LL`), como en macOS.
