/*
 * RawKVM VHF driver: user-mode interface.
 *
 * Shared by the driver (C) and mirrored in crates/agent-windows/src/ioctl.rs,
 * whose tests pin the byte layout below. Change both together and bump
 * RAWKVM_IOCTL_VERSION.
 *
 * Open \\.\RawKvmVhf (Administrators and SYSTEM only), then:
 *
 *   IOCTL_RAWKVM_CREATE   in:  RAWKVM_CREATE_HEADER + report descriptor
 *                         out: ULONG handle
 *   IOCTL_RAWKVM_REPORT   in:  ULONG handle + raw input report, exactly as
 *                              the device sends it (report ID byte first only
 *                              if the descriptor declares report IDs)
 *   IOCTL_RAWKVM_DESTROY  in:  ULONG handle
 *   IOCTL_RAWKVM_DESTROY_ALL  (no buffers)
 *
 * Every virtual device belongs to the file handle that created it and is
 * deleted when that handle is closed, so a crashed agent leaves nothing
 * behind.
 */
#pragma once

#define RAWKVM_DEVICE_TYPE      0x8000  /* vendor-defined device types start at 0x8000 */
#define RAWKVM_IOCTL_VERSION    1
#define RAWKVM_MAX_DESCRIPTOR   4096
#define RAWKVM_MAX_REPORT       1024
#define RAWKVM_MAX_DEVICES      32

/* CTL_CODE(RAWKVM_DEVICE_TYPE, function, METHOD_BUFFERED, FILE_WRITE_ACCESS) */
#define RAWKVM_CTL(fn) (((ULONG)RAWKVM_DEVICE_TYPE << 16) | (0x2 << 14) | ((fn) << 2) | 0)

#define IOCTL_RAWKVM_CREATE      RAWKVM_CTL(0x800)
#define IOCTL_RAWKVM_REPORT      RAWKVM_CTL(0x801)
#define IOCTL_RAWKVM_DESTROY     RAWKVM_CTL(0x802)
#define IOCTL_RAWKVM_DESTROY_ALL RAWKVM_CTL(0x803)

/* The descriptor declares report IDs: reports start with the ID byte. */
#define RAWKVM_FLAG_REPORT_IDS  0x01

#define RAWKVM_DOS_NAME L"\\DosDevices\\RawKvmVhf"

#pragma pack(push, 1)
typedef struct _RAWKVM_CREATE_HEADER {
    ULONG   Version;            /* RAWKVM_IOCTL_VERSION */
    USHORT  VendorId;
    USHORT  ProductId;
    USHORT  VersionNumber;
    USHORT  DescriptorLength;   /* bytes following this header */
    GUID    ContainerId;        /* same for all devices of one agent */
    UCHAR   DeviceIndex;        /* HidDevice.id, makes the instance id unique */
    UCHAR   Flags;              /* RAWKVM_FLAG_* */
    UCHAR   Reserved[2];
} RAWKVM_CREATE_HEADER;         /* 32 bytes */
#pragma pack(pop)
