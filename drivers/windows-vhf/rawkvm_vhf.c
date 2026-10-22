/*
 * RawKVM VHF driver.
 *
 * A root-enumerated KMDF function driver with vhf.sys as its lower filter
 * (see RawKvmVhf.inf). User mode (the RawKVM agent service) opens
 * \\.\RawKvmVhf and asks for virtual HID devices with arbitrary report
 * descriptors; each one is a VHF child whose input reports are submitted
 * verbatim. Nothing is interpreted: hidclass and kbdhid/mouhid see the
 * Logitech receiver as if it were plugged in. See docs/adr/0006.
 *
 * Structure follows Microsoft's VHF guidance ("Write a HID source driver by
 * using Virtual HID Framework") and the HIDInjector sample
 * (microsoft/Windows-IoT-Samples, samples/HIDInjector/driver).
 */

#include <ntddk.h>
#include <wdf.h>
#include <vhf.h>
#include <ntstrsafe.h>
#include "rawkvm_ioctl.h"

#define RAWKVM_POOL_TAG 'mvKR'

typedef struct _VHF_SLOT {
    ULONG           Handle;         /* 0 = free; (generation << 8) | index */
    WDFFILEOBJECT   Owner;
    VHFHANDLE       Vhf;
    BOOLEAN         ReportIds;
    PUCHAR          Descriptor;     /* kept alive while the VHF device exists */
    WCHAR           InstanceId[16];
} VHF_SLOT, *PVHF_SLOT;

typedef struct _DEVICE_CONTEXT {
    WDFWAITLOCK Lock;
    ULONG       Generation;
    VHF_SLOT    Slots[RAWKVM_MAX_DEVICES];
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, GetDeviceContext)

DRIVER_INITIALIZE                   DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD           RawKvmEvtDeviceAdd;
EVT_WDF_OBJECT_CONTEXT_CLEANUP      RawKvmEvtDeviceCleanup;
EVT_WDF_FILE_CLEANUP                RawKvmEvtFileCleanup;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL  RawKvmEvtIoDeviceControl;
EVT_VHF_ASYNC_OPERATION             RawKvmEvtVhfWriteReport;

/*
 * Output reports from the host (keyboard LEDs, HID++ requests from Logitech
 * software) are accepted and dropped: completing immediately keeps kbdhid
 * from retrying or logging errors. Feature and GET_INPUT_REPORT requests
 * have no callback, so VHF completes them with STATUS_NOT_SUPPORTED. No
 * operation is ever left pending.
 */
VOID
RawKvmEvtVhfWriteReport(
    _In_ PVOID VhfClientContext,
    _In_ VHFOPERATIONHANDLE VhfOperationHandle,
    _In_opt_ PVOID VhfOperationContext,
    _In_ PHID_XFER_PACKET HidTransferPacket)
{
    UNREFERENCED_PARAMETER(VhfClientContext);
    UNREFERENCED_PARAMETER(VhfOperationContext);
    UNREFERENCED_PARAMETER(HidTransferPacket);
    (VOID)VhfAsyncOperationComplete(VhfOperationHandle, STATUS_SUCCESS);
}

NTSTATUS
DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;

    WDF_DRIVER_CONFIG_INIT(&config, RawKvmEvtDeviceAdd);
    return WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES,
                           &config, WDF_NO_HANDLE);
}

NTSTATUS
RawKvmEvtDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    NTSTATUS status;
    WDFDEVICE device;
    PDEVICE_CONTEXT ctx;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_OBJECT_ATTRIBUTES queueAttributes;
    DECLARE_CONST_UNICODE_STRING(dosName, RAWKVM_DOS_NAME);

    UNREFERENCED_PARAMETER(Driver);
    PAGED_CODE();

    /* Track the agent's handle so its devices go away with it. */
    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, WDF_NO_EVENT_CALLBACK,
                               WDF_NO_EVENT_CALLBACK, RawKvmEvtFileCleanup);
    WdfDeviceInitSetFileObjectConfig(DeviceInit, &fileConfig, WDF_NO_OBJECT_ATTRIBUTES);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);
    attributes.EvtCleanupCallback = RawKvmEvtDeviceCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    ctx = GetDeviceContext(device);

    status = WdfWaitLockCreate(WDF_NO_OBJECT_ATTRIBUTES, &ctx->Lock);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* Access is limited to SYSTEM and Administrators by the INF. */
    status = WdfDeviceCreateSymbolicLink(device, &dosName);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    /* VhfCreate and VhfDelete need PASSIVE_LEVEL. */
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.EvtIoDeviceControl = RawKvmEvtIoDeviceControl;
    WDF_OBJECT_ATTRIBUTES_INIT(&queueAttributes);
    queueAttributes.ExecutionLevel = WdfExecutionLevelPassive;
    return WdfIoQueueCreate(device, &queueConfig, &queueAttributes, WDF_NO_HANDLE);
}

/* Take a slot out of the table under the lock; delete it outside. */
static VOID
RawKvmFreeSlot(_Inout_ PVHF_SLOT Detached)
{
    PAGED_CODE();
    if (Detached->Vhf != NULL) {
        /* Waits until PnP has removed the child; no new reports for it. */
        VhfDelete(Detached->Vhf, TRUE);
    }
    if (Detached->Descriptor != NULL) {
        ExFreePoolWithTag(Detached->Descriptor, RAWKVM_POOL_TAG);
    }
}

/* Delete every device owned by Owner (NULL: all). */
static VOID
RawKvmDestroyOwned(_In_ PDEVICE_CONTEXT Ctx, _In_opt_ WDFFILEOBJECT Owner)
{
    ULONG i;
    VHF_SLOT detached;

    PAGED_CODE();
    for (i = 0; i < RAWKVM_MAX_DEVICES; i++) {
        WdfWaitLockAcquire(Ctx->Lock, NULL);
        if (Ctx->Slots[i].Handle == 0 || (Owner != NULL && Ctx->Slots[i].Owner != Owner)) {
            WdfWaitLockRelease(Ctx->Lock);
            continue;
        }
        detached = Ctx->Slots[i];
        RtlZeroMemory(&Ctx->Slots[i], sizeof(VHF_SLOT));
        WdfWaitLockRelease(Ctx->Lock);
        RawKvmFreeSlot(&detached);
    }
}

VOID
RawKvmEvtFileCleanup(_In_ WDFFILEOBJECT FileObject)
{
    PAGED_CODE();
    RawKvmDestroyOwned(GetDeviceContext(WdfFileObjectGetDevice(FileObject)), FileObject);
}

VOID
RawKvmEvtDeviceCleanup(_In_ WDFOBJECT Object)
{
    PAGED_CODE();
    RawKvmDestroyOwned(GetDeviceContext((WDFDEVICE)Object), NULL);
}

static NTSTATUS
RawKvmCreate(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request, _Out_ size_t *Information)
{
    NTSTATUS status;
    PDEVICE_CONTEXT ctx = GetDeviceContext(Device);
    RAWKVM_CREATE_HEADER *hdr;
    size_t inLen;
    ULONG *out;
    ULONG i;
    PVHF_SLOT slot = NULL;
    VHF_CONFIG cfg;
    PUCHAR descriptor;
    ULONG handle;

    PAGED_CODE();
    *Information = 0;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(*hdr), (PVOID *)&hdr, &inLen);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (hdr->Version != RAWKVM_IOCTL_VERSION) {
        return STATUS_REVISION_MISMATCH;
    }
    if (hdr->DescriptorLength == 0 || hdr->DescriptorLength > RAWKVM_MAX_DESCRIPTOR ||
        inLen < sizeof(*hdr) + hdr->DescriptorLength) {
        return STATUS_INVALID_PARAMETER;
    }
    status = WdfRequestRetrieveOutputBuffer(Request, sizeof(ULONG), (PVOID *)&out, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    descriptor = (PUCHAR)ExAllocatePool2(POOL_FLAG_NON_PAGED, hdr->DescriptorLength, RAWKVM_POOL_TAG);
    if (descriptor == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlCopyMemory(descriptor, (PUCHAR)(hdr + 1), hdr->DescriptorLength);

    WdfWaitLockAcquire(ctx->Lock, NULL);
    for (i = 0; i < RAWKVM_MAX_DEVICES; i++) {
        if (ctx->Slots[i].Handle == 0) {
            slot = &ctx->Slots[i];
            break;
        }
    }
    if (slot == NULL) {
        WdfWaitLockRelease(ctx->Lock);
        ExFreePoolWithTag(descriptor, RAWKVM_POOL_TAG);
        return STATUS_TOO_MANY_NODES;
    }
    ctx->Generation = (ctx->Generation + 1) & 0x00FFFFFF;
    if (ctx->Generation == 0) {
        ctx->Generation = 1;
    }
    handle = (ctx->Generation << 8) | i;

    RtlZeroMemory(slot, sizeof(*slot));
    slot->Descriptor = descriptor;
    slot->ReportIds = (hdr->Flags & RAWKVM_FLAG_REPORT_IDS) != 0;
    (VOID)RtlStringCchPrintfW(slot->InstanceId, ARRAYSIZE(slot->InstanceId),
                              L"RAWKVM%02X", hdr->DeviceIndex);

    VHF_CONFIG_INIT(&cfg, WdfDeviceWdmGetDeviceObject(Device),
                    hdr->DescriptorLength, descriptor);
    cfg.VendorID = hdr->VendorId;
    cfg.ProductID = hdr->ProductId;
    cfg.VersionNumber = hdr->VersionNumber;
    cfg.ContainerID = hdr->ContainerId;
    cfg.InstanceID = slot->InstanceId;
    cfg.InstanceIDLength = (USHORT)(wcslen(slot->InstanceId) * sizeof(WCHAR));
    cfg.EvtVhfAsyncOperationWriteReport = RawKvmEvtVhfWriteReport;

    status = VhfCreate(&cfg, &slot->Vhf);
    if (NT_SUCCESS(status)) {
        status = VhfStart(slot->Vhf);
        if (!NT_SUCCESS(status)) {
            VhfDelete(slot->Vhf, TRUE);
            slot->Vhf = NULL;
        }
    }
    if (!NT_SUCCESS(status)) {
        RtlZeroMemory(slot, sizeof(*slot));
        WdfWaitLockRelease(ctx->Lock);
        ExFreePoolWithTag(descriptor, RAWKVM_POOL_TAG);
        return status;
    }
    slot->Handle = handle;
    slot->Owner = WdfRequestGetFileObject(Request);
    WdfWaitLockRelease(ctx->Lock);

    *out = handle;
    *Information = sizeof(ULONG);
    return STATUS_SUCCESS;
}

/* Slot for Handle if Owner owns it; call with the lock held. */
static PVHF_SLOT
RawKvmFind(_In_ PDEVICE_CONTEXT Ctx, _In_ ULONG Handle, _In_ WDFFILEOBJECT Owner)
{
    ULONG index = Handle & 0xFF;

    if (Handle == 0 || index >= RAWKVM_MAX_DEVICES) {
        return NULL;
    }
    if (Ctx->Slots[index].Handle != Handle || Ctx->Slots[index].Owner != Owner) {
        return NULL;
    }
    return &Ctx->Slots[index];
}

static NTSTATUS
RawKvmReport(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request)
{
    NTSTATUS status;
    PDEVICE_CONTEXT ctx = GetDeviceContext(Device);
    PUCHAR in;
    size_t inLen;
    ULONG handle;
    PVHF_SLOT slot;
    HID_XFER_PACKET packet;

    status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG) + 1, (PVOID *)&in, &inLen);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    if (inLen - sizeof(ULONG) > RAWKVM_MAX_REPORT) {
        return STATUS_INVALID_BUFFER_SIZE;
    }
    RtlCopyMemory(&handle, in, sizeof(ULONG));

    /*
     * The buffer is the report as the device sends it on the wire, which is
     * what VHF (acting as the transport minidriver) hands to hidclass: with
     * the report ID byte first when the descriptor declares IDs, without it
     * otherwise (hidclass then prefixes 0 for its clients itself).
     */
    packet.reportBuffer = in + sizeof(ULONG);
    packet.reportBufferLen = (ULONG)(inLen - sizeof(ULONG));

    WdfWaitLockAcquire(ctx->Lock, NULL);
    slot = RawKvmFind(ctx, handle, WdfRequestGetFileObject(Request));
    if (slot == NULL) {
        status = STATUS_INVALID_HANDLE;
    } else {
        packet.reportId = slot->ReportIds ? packet.reportBuffer[0] : 0;
        /* VHF copies the report; the buffer may be reused on return. */
        status = VhfReadReportSubmit(slot->Vhf, &packet);
    }
    WdfWaitLockRelease(ctx->Lock);
    return status;
}

static NTSTATUS
RawKvmDestroy(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request)
{
    NTSTATUS status;
    PDEVICE_CONTEXT ctx = GetDeviceContext(Device);
    ULONG *handle;
    PVHF_SLOT slot;
    VHF_SLOT detached;

    PAGED_CODE();
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(ULONG), (PVOID *)&handle, NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    WdfWaitLockAcquire(ctx->Lock, NULL);
    slot = RawKvmFind(ctx, *handle, WdfRequestGetFileObject(Request));
    if (slot == NULL) {
        WdfWaitLockRelease(ctx->Lock);
        return STATUS_INVALID_HANDLE;
    }
    detached = *slot;
    RtlZeroMemory(slot, sizeof(*slot));
    WdfWaitLockRelease(ctx->Lock);
    RawKvmFreeSlot(&detached);
    return STATUS_SUCCESS;
}

VOID
RawKvmEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    NTSTATUS status;
    size_t information = 0;

    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(InputBufferLength);
    PAGED_CODE();

    switch (IoControlCode) {
    case IOCTL_RAWKVM_CREATE:
        status = RawKvmCreate(device, Request, &information);
        break;
    case IOCTL_RAWKVM_REPORT:
        status = RawKvmReport(device, Request);
        break;
    case IOCTL_RAWKVM_DESTROY:
        status = RawKvmDestroy(device, Request);
        break;
    case IOCTL_RAWKVM_DESTROY_ALL:
        RawKvmDestroyOwned(GetDeviceContext(device), WdfRequestGetFileObject(Request));
        status = STATUS_SUCCESS;
        break;
    default:
        status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }
    WdfRequestCompleteWithInformation(Request, status, information);
}
