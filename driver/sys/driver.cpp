/*
 * MicHost virtual microphone - PortCls adapter, control device, and shared ring.
 *
 * This is a capture-only WaveRT device.  There is no render endpoint.
 */

#include "driver.h"

MH_VMIC_STATE g_MhVmic;

#define MH_SAFE_RELEASE(p) do { if ((p) != NULL) { (p)->Release(); (p) = NULL; } } while (0)

#ifndef SECTION_ALL_ACCESS
#define SECTION_ALL_ACCESS (STANDARD_RIGHTS_REQUIRED | SECTION_QUERY | SECTION_MAP_WRITE | SECTION_MAP_READ | SECTION_EXTEND_SIZE)
#endif

/* ------------------------------------------------------------------------- */
/* Allocation                                                                */
/* ------------------------------------------------------------------------- */

void *__cdecl operator new(size_t Size)
{
    return ExAllocatePool2(POOL_FLAG_NON_PAGED, Size, MH_VMIC_POOL_TAG);
}

void *__cdecl operator new(size_t Size, POOL_FLAGS Flags, ULONG Tag)
{
    return ExAllocatePool2(Flags, Size, Tag);
}

void __cdecl operator delete(void *Pointer)
{
    if (Pointer != NULL)
    {
        ExFreePool(Pointer);
    }
}

void __cdecl operator delete(void *Pointer, size_t)
{
    if (Pointer != NULL)
    {
        ExFreePool(Pointer);
    }
}

/* ------------------------------------------------------------------------- */
/* Forward declarations                                                      */
/* ------------------------------------------------------------------------- */

DRIVER_INITIALIZE DriverEntry;
DRIVER_ADD_DEVICE MhVmicAddDevice;
static NTSTATUS MhVmicStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                                  _In_ PRESOURCELIST ResourceList);
static NTSTATUS MhVmicCreateControlDevice(_In_ PDRIVER_OBJECT DriverObject);
static VOID MhVmicDestroyControlDevice(VOID);
static VOID MhVmicRemoveAdapterState(VOID);
static VOID MhVmicDriverUnload(_In_ PDRIVER_OBJECT DriverObject);

static NTSTATUS MhVmicDispatchCreate(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp);
static NTSTATUS MhVmicDispatchClose(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp);
static NTSTATUS MhVmicDispatchCleanup(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp);
static NTSTATUS MhVmicDispatchDeviceControl(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp);
static NTSTATUS MhVmicDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp);

/* ------------------------------------------------------------------------- */
/* Power management                                                          */
/* ------------------------------------------------------------------------- */

class CAdapterContext : public IAdapterPowerManagement, public CUnknown
{
public:
    CAdapterContext(_In_opt_ PUNKNOWN OuterUnknown)
        : CUnknown(OuterUnknown),
          m_PowerState(PowerDeviceD0)
    {
    }

    DECLARE_STD_UNKNOWN();
    IMP_IAdapterPowerManagement;

private:
    POWER_STATE m_PowerState;
};

STDMETHODIMP
CAdapterContext::NonDelegatingQueryInterface(_In_ REFIID Interface, _Out_ PVOID *Object)
{
    PAGED_CODE();

    if (Object == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    if (IsEqualGUIDAligned(Interface, IID_IUnknown))
    {
        *Object = PVOID(static_cast<IUnknown *>(static_cast<IAdapterPowerManagement *>(this)));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IAdapterPowerManagement))
    {
        *Object = PVOID(static_cast<IAdapterPowerManagement *>(this));
    }
    else
    {
        *Object = NULL;
        return STATUS_INVALID_PARAMETER;
    }

    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CAdapterContext::PowerChangeState(_In_ POWER_STATE NewState)
{
    PAGED_CODE();

    m_PowerState = NewState;
    if (NewState.DeviceState == PowerDeviceD3)
    {
        MhVmicWavePowerStop();
    }
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CAdapterContext::QueryPowerChangeState(_In_ POWER_STATE NewStateQuery)
{
    UNREFERENCED_PARAMETER(NewStateQuery);
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CAdapterContext::QueryDeviceCapabilities(_In_ PDEVICE_CAPABILITIES Capabilities)
{
    UNREFERENCED_PARAMETER(Capabilities);
    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------- */
/* Section / ring                                                            */
/* ------------------------------------------------------------------------- */

static VOID MhGenerateSectionName(VOID)
{
    ULONG Words[4] = { 0, 0, 0, 0 };

    /* The name is the only capability that leads to the section, so do not
       derive it from a kernel address.  Fall back to the clock only if the
       RNG is unavailable. */
    if (!NT_SUCCESS(BCryptGenRandom(NULL, (PUCHAR)Words, sizeof(Words),
        BCRYPT_USE_SYSTEM_PREFERRED_RNG)))
    {
        LARGE_INTEGER Qpc = KeQueryPerformanceCounter(NULL);
        Words[0] = (ULONG)Qpc.LowPart;
        Words[1] = (ULONG)Qpc.HighPart;
        Words[2] = (ULONG)(Qpc.QuadPart >> 16);
        Words[3] = (ULONG)Qpc.QuadPart;
    }

    RtlStringCchPrintfW(g_MhVmic.SectionNameBuffer, MH_VMIC_SECTION_NAME_CHARS,
        L"MicHostVirtualMic-%08lX%08lX%08lX%08lX",
        Words[0], Words[1], Words[2], Words[3]);
    RtlInitUnicodeString(&g_MhVmic.SectionName, g_MhVmic.SectionNameBuffer);
}

static VOID MhResetRingLocked(_In_ MH_VMIC_RING *Ring)
{
    ULONG DataBytes;

    if (Ring == NULL)
    {
        return;
    }

    DataBytes = g_MhVmic.RingFrames * (ULONG)g_MhVmic.Channels * MH_VMIC_BYTES_PER_SAMPLE;
    RtlZeroMemory(Ring, sizeof(*Ring));
    RtlZeroMemory(MH_VMIC_RING_DATA(Ring), DataBytes);

    Ring->magic = MH_VMIC_MAGIC;
    Ring->version = MH_VMIC_VERSION;
    Ring->sample_rate = (uint32_t)g_MhVmic.SampleRate;
    Ring->channels = (uint32_t)g_MhVmic.Channels;
    Ring->bytes_per_sample = MH_VMIC_BYTES_PER_SAMPLE;
    Ring->packet_frames = (uint32_t)g_MhVmic.PacketFrames;
    Ring->ring_frames = g_MhVmic.RingFrames;
    Ring->writer_active = 0;
    Ring->write_index = 0;
    Ring->read_index = 0;
    Ring->overruns = 0;
    Ring->underruns = 0;
    Ring->qpc_frequency = g_MhVmic.QpcFrequency;
    Ring->last_write_qpc = 0;
}

static VOID MhDestroySectionLocked(VOID)
{
    InterlockedExchangePointer((PVOID volatile *)&g_MhVmic.Ring, NULL);
    KeMemoryBarrier();

    if (g_MhVmic.RingMdl != NULL)
    {
        if (g_MhVmic.RingMdlLocked)
        {
            __try
            {
                MmUnlockPages(g_MhVmic.RingMdl);
            }
            __except (EXCEPTION_EXECUTE_HANDLER)
            {
            }
            g_MhVmic.RingMdlLocked = FALSE;
        }
        IoFreeMdl(g_MhVmic.RingMdl);
        g_MhVmic.RingMdl = NULL;
    }

    if (g_MhVmic.SectionView != NULL)
    {
        ZwUnmapViewOfSection(ZwCurrentProcess(), g_MhVmic.SectionView);
        g_MhVmic.SectionView = NULL;
    }

    if (g_MhVmic.SectionHandle != NULL)
    {
        ZwClose(g_MhVmic.SectionHandle);
        g_MhVmic.SectionHandle = NULL;
    }

    g_MhVmic.RingFrames = 0;
    g_MhVmic.SectionBytes = 0;
}

static ULONG MhVmicSidSize(_In_ ULONG SubAuthorityCount)
{
    return (ULONG)(sizeof(SID) - sizeof(ULONG) +
        (SIZE_T)SubAuthorityCount * sizeof(ULONG));
}

static VOID MhVmicInitializeSid(_Out_ PSID Sid, _In_ ULONG SubAuthorityCount,
                                _In_reads_(SubAuthorityCount) const ULONG *SubAuthorities)
{
    SID_IDENTIFIER_AUTHORITY NtAuthority = SECURITY_NT_AUTHORITY;
    ULONG Index;

    RtlZeroMemory(Sid, MhVmicSidSize(SubAuthorityCount));
    Sid->Revision = SID_REVISION;
    Sid->SubAuthorityCount = (UCHAR)SubAuthorityCount;
    RtlCopyMemory(&Sid->IdentifierAuthority, &NtAuthority, sizeof(NtAuthority));
    for (Index = 0; Index < SubAuthorityCount; ++Index)
    {
        Sid->SubAuthority[Index] = SubAuthorities[Index];
    }
}

static NTSTATUS MhVmicBuildSectionSecurityDescriptor(_Out_ PSECURITY_DESCRIPTOR *OutDescriptor,
                                                      _Out_ PACL *OutAcl)
{
    static const ULONG SystemSubAuthorities[] = { SECURITY_LOCAL_SYSTEM_RID };
    static const ULONG AdminSubAuthorities[] =
        { SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS };
    static const ULONG InteractiveSubAuthorities[] = { SECURITY_INTERACTIVE_RID };
    UCHAR SystemSidBuffer[SECURITY_MAX_SID_SIZE];
    UCHAR AdminSidBuffer[SECURITY_MAX_SID_SIZE];
    UCHAR InteractiveSidBuffer[SECURITY_MAX_SID_SIZE];
    PSID SystemSid = (PSID)SystemSidBuffer;
    PSID AdminSid = (PSID)AdminSidBuffer;
    PSID InteractiveSid = (PSID)InteractiveSidBuffer;
    const PSID Sids[3] = { SystemSid, AdminSid, InteractiveSid };
    ULONG SidSizes[3];
    ULONG AclBytes;
    PACL Acl;
    PSECURITY_DESCRIPTOR Descriptor;
    NTSTATUS Status;
    ULONG Index;
    PUCHAR AcePosition;

    *OutDescriptor = NULL;
    *OutAcl = NULL;

    MhVmicInitializeSid(SystemSid, RTL_NUMBER_OF(SystemSubAuthorities), SystemSubAuthorities);
    MhVmicInitializeSid(AdminSid, RTL_NUMBER_OF(AdminSubAuthorities), AdminSubAuthorities);
    MhVmicInitializeSid(InteractiveSid, RTL_NUMBER_OF(InteractiveSubAuthorities),
        InteractiveSubAuthorities);

    SidSizes[0] = MhVmicSidSize(RTL_NUMBER_OF(SystemSubAuthorities));
    SidSizes[1] = MhVmicSidSize(RTL_NUMBER_OF(AdminSubAuthorities));
    SidSizes[2] = MhVmicSidSize(RTL_NUMBER_OF(InteractiveSubAuthorities));

    AclBytes = sizeof(ACL);
    for (Index = 0; Index < 3; ++Index)
    {
        AclBytes += (ULONG)(sizeof(ACCESS_ALLOWED_ACE) - sizeof(ULONG)) + SidSizes[Index];
    }

    Acl = (PACL)ExAllocatePool2(POOL_FLAG_NON_PAGED, AclBytes, MH_VMIC_POOL_TAG);
    if (Acl == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Acl->AclRevision = ACL_REVISION;
    Acl->Sbz1 = 0;
    Acl->AclSize = (USHORT)AclBytes;
    Acl->AceCount = 3;
    Acl->Sbz2 = 0;

    AcePosition = (PUCHAR)Acl + sizeof(ACL);
    for (Index = 0; Index < 3; ++Index)
    {
        PACCESS_ALLOWED_ACE Ace = (PACCESS_ALLOWED_ACE)AcePosition;
        ULONG AceBytes = (ULONG)(sizeof(ACCESS_ALLOWED_ACE) - sizeof(ULONG)) + SidSizes[Index];

        Ace->Header.AceType = ACCESS_ALLOWED_ACE_TYPE;
        Ace->Header.AceFlags = 0;
        Ace->Header.AceSize = (USHORT)AceBytes;
        Ace->Mask = SECTION_ALL_ACCESS;
        RtlCopyMemory(&Ace->SidStart, Sids[Index], SidSizes[Index]);
        AcePosition += AceBytes;
    }

    Descriptor = (PSECURITY_DESCRIPTOR)ExAllocatePool2(
        POOL_FLAG_NON_PAGED, SECURITY_DESCRIPTOR_MIN_LENGTH, MH_VMIC_POOL_TAG);
    if (Descriptor == NULL)
    {
        ExFreePool(Acl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Status = RtlCreateSecurityDescriptor(Descriptor, SECURITY_DESCRIPTOR_REVISION);
    if (NT_SUCCESS(Status))
    {
        Status = RtlSetDaclSecurityDescriptor(Descriptor, TRUE, Acl, FALSE);
    }
    if (!NT_SUCCESS(Status))
    {
        ExFreePool(Descriptor);
        ExFreePool(Acl);
        return Status;
    }

    *OutDescriptor = Descriptor;
    *OutAcl = Acl;
    return STATUS_SUCCESS;
}

static NTSTATUS MhCreateSectionLocked(_In_ ULONG SampleRate, _In_ ULONG Channels,
                                      _In_ ULONG PacketFrames, _In_ ULONG SessionId)
{
    NTSTATUS Status;
    ULONG RingFrames;
    ULONG SectionBytes;
    WCHAR FullName[96];
    UNICODE_STRING FullNameString;
    OBJECT_ATTRIBUTES ObjectAttributes;
    PSECURITY_DESCRIPTOR SecurityDescriptor = NULL;
    PACL SecurityAcl = NULL;
    LARGE_INTEGER MaximumSize;
    HANDLE SectionHandle = NULL;
    PVOID SectionView = NULL;
    SIZE_T ViewSize = 0;
    PMDL RingMdl = NULL;
    PVOID RingBase = NULL;
    BOOLEAN Locked = FALSE;
    MH_VMIC_RING *Ring;

    if (PacketFrames < MH_VMIC_MIN_PACKET_FRAMES || PacketFrames > MH_VMIC_MAX_PACKET_FRAMES)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (Channels < 1 || Channels > 2)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (SampleRate < 8000 || SampleRate > 192000)
    {
        return STATUS_INVALID_PARAMETER;
    }

    RingFrames = MhVmicRingFramesForPacket(PacketFrames);
    if (RingFrames < PacketFrames * 2)
    {
        return STATUS_INVALID_PARAMETER;
    }
    SectionBytes = (ULONG)MH_VMIC_SECTION_BYTES(RingFrames, Channels);
    if (SectionBytes == 0 || SectionBytes > (64u * 1024u * 1024u))
    {
        return STATUS_INVALID_PARAMETER;
    }

    /* Reuse the existing section when the format is unchanged. */
    if (g_MhVmic.Ring != NULL &&
        g_MhVmic.RingFrames == RingFrames &&
        g_MhVmic.SampleRate == (LONG)SampleRate &&
        g_MhVmic.Channels == (LONG)Channels &&
        g_MhVmic.SessionId == SessionId)
    {
        g_MhVmic.PacketFrames = (LONG)PacketFrames;
        g_MhVmic.QpcFrequency = MhVmicQpcFrequency();
        MhResetRingLocked(g_MhVmic.Ring);
        return STATUS_SUCCESS;
    }

    /* A live stream owns the old mapping.  Do not tear it down underneath it. */
    if (InterlockedCompareExchange(&g_MhVmic.StreamCount, 0, 0) > 0 ||
        InterlockedCompareExchange(&g_MhVmic.ConsumerActive, 0, 0) != 0)
    {
        return STATUS_DEVICE_BUSY;
    }

    MhDestroySectionLocked();
    MhGenerateSectionName();

    g_MhVmic.SampleRate = (LONG)SampleRate;
    g_MhVmic.Channels = (LONG)Channels;
    g_MhVmic.PacketFrames = (LONG)PacketFrames;
    g_MhVmic.RingFrames = RingFrames;
    g_MhVmic.SectionBytes = SectionBytes;
    g_MhVmic.SessionId = SessionId;
    g_MhVmic.QpcFrequency = MhVmicQpcFrequency();

    RtlStringCchPrintfW(FullName, RTL_NUMBER_OF(FullName), MH_VMIC_SESSION_OBJECTS_U,
        SessionId, g_MhVmic.SectionNameBuffer);
    RtlInitUnicodeString(&FullNameString, FullName);

    Status = MhVmicBuildSectionSecurityDescriptor(&SecurityDescriptor, &SecurityAcl);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    InitializeObjectAttributes(&ObjectAttributes, &FullNameString,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, NULL, SecurityDescriptor);
    MaximumSize.QuadPart = SectionBytes;
    Status = ZwCreateSection(&SectionHandle, SECTION_ALL_ACCESS, &ObjectAttributes,
        &MaximumSize, PAGE_READWRITE, SEC_COMMIT, NULL);
    ExFreePool(SecurityAcl);
    ExFreePool(SecurityDescriptor);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    Status = ZwMapViewOfSection(SectionHandle, ZwCurrentProcess(), &SectionView,
        0, 0, NULL, &ViewSize, ViewUnmap, 0, PAGE_READWRITE);
    if (!NT_SUCCESS(Status))
    {
        ZwClose(SectionHandle);
        return Status;
    }

    RingMdl = IoAllocateMdl(SectionView, (ULONG)ViewSize, FALSE, FALSE, NULL);
    if (RingMdl == NULL)
    {
        ZwUnmapViewOfSection(ZwCurrentProcess(), SectionView);
        ZwClose(SectionHandle);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    __try
    {
        MmProbeAndLockPages(RingMdl, KernelMode, IoModifyAccess);
        Locked = TRUE;
    }
    __except (EXCEPTION_EXECUTE_HANDLER)
    {
        Locked = FALSE;
    }
    if (!Locked)
    {
        IoFreeMdl(RingMdl);
        ZwUnmapViewOfSection(ZwCurrentProcess(), SectionView);
        ZwClose(SectionHandle);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RingBase = MmGetSystemAddressForMdlSafe(RingMdl, NormalPagePriority);
    if (RingBase == NULL)
    {
        MmUnlockPages(RingMdl);
        IoFreeMdl(RingMdl);
        ZwUnmapViewOfSection(ZwCurrentProcess(), SectionView);
        ZwClose(SectionHandle);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Ring = (MH_VMIC_RING *)RingBase;
    MhResetRingLocked(Ring);

    g_MhVmic.SectionHandle = SectionHandle;
    g_MhVmic.SectionView = SectionView;
    g_MhVmic.RingMdl = RingMdl;
    g_MhVmic.RingMdlLocked = TRUE;
    InterlockedExchangePointer((PVOID volatile *)&g_MhVmic.Ring, Ring);
    KeMemoryBarrier();

    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------- */
/* Writer lifecycle                                                          */
/* ------------------------------------------------------------------------- */

static BOOLEAN MhReleaseWriterLocked(_In_opt_ PFILE_OBJECT FileObject,
                                     _In_ BOOLEAN Force)
{
    MH_VMIC_RING *Ring;

    if (!Force && (FileObject == NULL || FileObject != g_MhVmic.WriterFile))
    {
        return FALSE;
    }

    Ring = MhVmicRingAcquire();
    if (Ring != NULL)
    {
        InterlockedExchange((volatile LONG *)&Ring->writer_active, 0);
        KeMemoryBarrier();
        if (g_MhVmic.RingFrames != 0)
        {
            RtlZeroMemory(MH_VMIC_RING_DATA(Ring),
                g_MhVmic.RingFrames * (ULONG)g_MhVmic.Channels * MH_VMIC_BYTES_PER_SAMPLE);
        }
        InterlockedExchange64((volatile LONG64 *)&Ring->read_index,
            (LONG64)InterlockedCompareExchange64((volatile LONG64 *)&Ring->write_index, 0, 0));
    }

    InterlockedExchange(&g_MhVmic.WriterActive, 0);
    g_MhVmic.WriterFile = NULL;
    return TRUE;
}

VOID MhVmicReleaseWriter(_In_ PFILE_OBJECT FileObject)
{
    KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
    MhReleaseWriterLocked(FileObject, FALSE);
    KeReleaseMutex(&g_MhVmic.Mutex, FALSE);
}

VOID MhVmicStreamOpened(VOID)
{
    InterlockedIncrement(&g_MhVmic.StreamCount);
}

VOID MhVmicStreamClosed(VOID)
{
    InterlockedDecrement(&g_MhVmic.StreamCount);
}

VOID MhVmicSetConsumerActive(_In_ BOOLEAN Active)
{
    InterlockedExchange(&g_MhVmic.ConsumerActive, Active ? 1 : 0);
}

BOOLEAN MhVmicConsumerActive(VOID)
{
    return InterlockedCompareExchange(&g_MhVmic.ConsumerActive, 0, 0) != 0;
}

ULONG MhVmicCurrentSampleRate(VOID)
{
    return (ULONG)InterlockedCompareExchange(&g_MhVmic.SampleRate, 0, 0);
}

ULONG MhVmicCurrentChannels(VOID)
{
    return (ULONG)InterlockedCompareExchange(&g_MhVmic.Channels, 0, 0);
}

ULONG MhVmicCurrentPacketFrames(VOID)
{
    return (ULONG)InterlockedCompareExchange(&g_MhVmic.PacketFrames, 0, 0);
}

MH_VMIC_RING *MhVmicRingAcquire(VOID)
{
    return (MH_VMIC_RING *)InterlockedCompareExchangePointer(
        (PVOID volatile *)&g_MhVmic.Ring, NULL, NULL);
}

/* ------------------------------------------------------------------------- */
/* Control device                                                            */
/* ------------------------------------------------------------------------- */

static const GUID MhVmicMediaClassGuid =
    { 0x4d36e96c, 0xe325, 0x11ce, { 0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18 } };

static NTSTATUS MhVmicCreateControlDevice(_In_ PDRIVER_OBJECT DriverObject)
{
    NTSTATUS Status;
    UNICODE_STRING DeviceName;
    UNICODE_STRING SymbolicLink;
    PDEVICE_OBJECT DeviceObject = NULL;

    if (g_MhVmic.ControlDevice != NULL)
    {
        return STATUS_SUCCESS;
    }

    RtlInitUnicodeString(&DeviceName, MH_VMIC_DEVICE_NAME_U);
    Status = IoCreateDeviceSecure(DriverObject, 0, &DeviceName, FILE_DEVICE_UNKNOWN,
        FILE_DEVICE_SECURE_OPEN, FALSE, MH_VMIC_CONTROL_SDDL,
        &MhVmicMediaClassGuid, &DeviceObject);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    DeviceObject->Flags &= ~DO_DEVICE_INITIALIZING;

    RtlInitUnicodeString(&SymbolicLink, MH_VMIC_SYMLINK_NAME_U);
    Status = IoCreateSymbolicLink(&SymbolicLink, &DeviceName);
    if (!NT_SUCCESS(Status))
    {
        IoDeleteDevice(DeviceObject);
        return Status;
    }

    g_MhVmic.ControlDevice = DeviceObject;
    g_MhVmic.ControlSymbolicLinkCreated = TRUE;
    return STATUS_SUCCESS;
}

static VOID MhVmicDestroyControlDevice(VOID)
{
    if (g_MhVmic.ControlSymbolicLinkCreated)
    {
        UNICODE_STRING SymbolicLink;
        RtlInitUnicodeString(&SymbolicLink, MH_VMIC_SYMLINK_NAME_U);
        IoDeleteSymbolicLink(&SymbolicLink);
        g_MhVmic.ControlSymbolicLinkCreated = FALSE;
    }

    if (g_MhVmic.ControlDevice != NULL)
    {
        IoDeleteDevice(g_MhVmic.ControlDevice);
        g_MhVmic.ControlDevice = NULL;
    }
}

static VOID MhVmicRemoveAdapterState(VOID)
{
    KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
    MhReleaseWriterLocked(NULL, TRUE);
    MhDestroySectionLocked();
    if (g_MhVmic.AdapterPower != NULL)
    {
        PUNKNOWN Power = g_MhVmic.AdapterPower;
        g_MhVmic.AdapterPower = NULL;
        Power->Release();
    }
    MhVmicDestroyControlDevice();
    KeReleaseMutex(&g_MhVmic.Mutex, FALSE);
}

static NTSTATUS MhVmicComplete(_In_ PIRP Irp, _In_ NTSTATUS Status, _In_ ULONG_PTR Information)
{
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = Information;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
}

static NTSTATUS MhVmicControlCreate(_In_ PIRP Irp)
{
    return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS MhVmicControlCleanup(_In_ PIRP Irp)
{
    PIO_STACK_LOCATION Stack = IoGetCurrentIrpStackLocation(Irp);
    MhVmicReleaseWriter(Stack->FileObject);
    return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS MhVmicControlDeviceControl(_In_ PIRP Irp)
{
    PIO_STACK_LOCATION Stack = IoGetCurrentIrpStackLocation(Irp);
    ULONG Code = Stack->Parameters.DeviceIoControl.IoControlCode;
    ULONG InputLength = Stack->Parameters.DeviceIoControl.InputBufferLength;
    ULONG OutputLength = Stack->Parameters.DeviceIoControl.OutputBufferLength;
    PVOID Buffer = Irp->AssociatedIrp.SystemBuffer;
    NTSTATUS Status = STATUS_INVALID_DEVICE_REQUEST;
    ULONG_PTR Information = 0;

    switch (Code)
    {
    case MH_VMIC_IOCTL_GET_VERSION:
        if (OutputLength < sizeof(uint32_t))
        {
            Status = STATUS_BUFFER_TOO_SMALL;
        }
        else
        {
            *(uint32_t *)Buffer = MH_VMIC_VERSION;
            Information = sizeof(uint32_t);
            Status = STATUS_SUCCESS;
        }
        break;

    case MH_VMIC_IOCTL_OPEN_WRITER:
        if (InputLength < sizeof(MH_VMIC_OPEN_WRITER))
        {
            Status = STATUS_BUFFER_TOO_SMALL;
            break;
        }
        {
            MH_VMIC_OPEN_WRITER *Request = (MH_VMIC_OPEN_WRITER *)Buffer;
            ULONG SampleRate = Request->SampleRate != 0 ? Request->SampleRate : MH_VMIC_DEFAULT_SAMPLE_RATE;
            ULONG Channels = Request->Channels != 0 ? Request->Channels : MH_VMIC_DEFAULT_CHANNELS;
            ULONG PacketFrames = Request->PacketFrames != 0
                ? Request->PacketFrames : MhVmicCurrentPacketFrames();
            PIO_STACK_LOCATION Current = IoGetCurrentIrpStackLocation(Irp);
            ULONG SessionId = 0;

            if (PacketFrames == 0)
            {
                PacketFrames = MH_VMIC_DEFAULT_PACKET_FRAMES;
            }
            if (MhVmicConsumerActive())
            {
                ULONG ActivePacket = (ULONG)MhVmicCurrentPacketFrames();
                if (ActivePacket != 0)
                {
                    PacketFrames = ActivePacket;
                }
            }
            if (SampleRate != MH_VMIC_DEFAULT_SAMPLE_RATE)
            {
                Status = STATUS_NOT_SUPPORTED;
                break;
            }
            (VOID)IoGetRequestorSessionId(Irp, &SessionId);

            KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
            if (InterlockedCompareExchange(&g_MhVmic.WriterActive, 0, 0) != 0 ||
                g_MhVmic.WriterFile != NULL)
            {
                Status = STATUS_DEVICE_BUSY;
            }
            else if (MhVmicConsumerActive() &&
                     (g_MhVmic.Ring == NULL ||
                      g_MhVmic.SampleRate != (LONG)SampleRate ||
                      g_MhVmic.Channels != (LONG)Channels))
            {
                Status = STATUS_DEVICE_BUSY;
            }
            else
            {
                Status = MhCreateSectionLocked(SampleRate, Channels, PacketFrames, SessionId);
                if (NT_SUCCESS(Status))
                {
                    MH_VMIC_RING *Ring = MhVmicRingAcquire();
                    if (Ring != NULL)
                    {
                        InterlockedExchange((volatile LONG *)&Ring->writer_active, 1);
                        KeMemoryBarrier();
                    }
                    g_MhVmic.WriterFile = Current->FileObject;
                    InterlockedExchange(&g_MhVmic.WriterActive, 1);
                }
            }
            KeReleaseMutex(&g_MhVmic.Mutex, FALSE);
        }
        break;

    case MH_VMIC_IOCTL_CLOSE_WRITER:
        {
            PIO_STACK_LOCATION Current = IoGetCurrentIrpStackLocation(Irp);
            KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
            if (!MhReleaseWriterLocked(Current->FileObject, FALSE))
            {
                Status = STATUS_INVALID_DEVICE_STATE;
            }
            else
            {
                Status = STATUS_SUCCESS;
            }
            KeReleaseMutex(&g_MhVmic.Mutex, FALSE);
        }
        break;

    case MH_VMIC_IOCTL_GET_STATS:
        if (OutputLength < sizeof(MH_VMIC_STATS))
        {
            Status = STATUS_BUFFER_TOO_SMALL;
        }
        else
        {
            MH_VMIC_STATS *Stats = (MH_VMIC_STATS *)Buffer;
            MH_VMIC_RING *Ring = MhVmicRingAcquire();

            RtlZeroMemory(Stats, sizeof(*Stats));
            Stats->ConsumerActive = MhVmicConsumerActive() ? 1u : 0u;
            Stats->QpcFrequency = g_MhVmic.QpcFrequency;
            if (Ring != NULL)
            {
                Stats->Underruns = (uint64_t)InterlockedCompareExchange64(
                    (volatile LONG64 *)&Ring->underruns, 0, 0);
                Stats->ReadIndex = (uint64_t)InterlockedCompareExchange64(
                    (volatile LONG64 *)&Ring->read_index, 0, 0);
            }
            Information = sizeof(*Stats);
            Status = STATUS_SUCCESS;
        }
        break;

    case MH_VMIC_IOCTL_SET_PACKET_FRAMES:
        if (InputLength < sizeof(MH_VMIC_SET_PACKET_FRAMES))
        {
            Status = STATUS_BUFFER_TOO_SMALL;
            break;
        }
        {
            ULONG PacketFrames = ((MH_VMIC_SET_PACKET_FRAMES *)Buffer)->PacketFrames;

            if (PacketFrames < MH_VMIC_MIN_PACKET_FRAMES ||
                PacketFrames > MH_VMIC_MAX_PACKET_FRAMES)
            {
                Status = STATUS_INVALID_PARAMETER;
                break;
            }

            KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
            if (InterlockedCompareExchange(&g_MhVmic.StreamCount, 0, 0) != 0)
            {
                Status = STATUS_INVALID_DEVICE_STATE;
            }
            else if (g_MhVmic.RingFrames != 0 && g_MhVmic.RingFrames < PacketFrames * 2)
            {
                Status = STATUS_INVALID_PARAMETER;
            }
            else
            {
                g_MhVmic.PacketFrames = (LONG)PacketFrames;
                if (g_MhVmic.Ring != NULL)
                {
                    InterlockedExchange((volatile LONG *)&g_MhVmic.Ring->packet_frames,
                        (LONG)PacketFrames);
                }
                Status = STATUS_SUCCESS;
            }
            KeReleaseMutex(&g_MhVmic.Mutex, FALSE);
        }
        break;

    case MH_VMIC_IOCTL_GET_SECTION_INFO:
        {
            PIO_STACK_LOCATION Current = IoGetCurrentIrpStackLocation(Irp);
            MH_VMIC_RING *Ring = MhVmicRingAcquire();
            ULONG NameBytes = g_MhVmic.SectionName.Length;

            if (g_MhVmic.WriterFile == NULL || Current->FileObject != g_MhVmic.WriterFile)
            {
                Status = STATUS_ACCESS_DENIED;
            }
            else if (Ring == NULL)
            {
                Status = STATUS_INVALID_DEVICE_STATE;
            }
            else if (OutputLength < sizeof(MH_VMIC_SECTION_INFO) + NameBytes)
            {
                Status = STATUS_BUFFER_TOO_SMALL;
            }
            else
            {
                MH_VMIC_SECTION_INFO *Info = (MH_VMIC_SECTION_INFO *)Buffer;
                RtlZeroMemory(Info, sizeof(*Info));
                Info->RingFrames = g_MhVmic.RingFrames;
                Info->PacketFrames = (ULONG)MhVmicCurrentPacketFrames();
                Info->SampleRate = (ULONG)MhVmicCurrentSampleRate();
                Info->Channels = (ULONG)MhVmicCurrentChannels();
                Info->NameBytes = NameBytes;
                RtlCopyMemory((PUCHAR)Buffer + sizeof(*Info),
                    g_MhVmic.SectionName.Buffer, NameBytes);
                Information = sizeof(*Info) + NameBytes;
                Status = STATUS_SUCCESS;
            }
        }
        break;

    default:
        Status = STATUS_INVALID_DEVICE_REQUEST;
        break;
    }

    return MhVmicComplete(Irp, Status, Information);
}

/* ------------------------------------------------------------------------- */
/* Dispatch wrappers                                                         */
/* ------------------------------------------------------------------------- */

static BOOLEAN MhIsControlDevice(_In_ PDEVICE_OBJECT DeviceObject)
{
    return DeviceObject != NULL && DeviceObject == g_MhVmic.ControlDevice;
}

static NTSTATUS MhVmicDispatchCreate(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    if (MhIsControlDevice(DeviceObject))
    {
        return MhVmicControlCreate(Irp);
    }
    if (g_MhVmic.PcCreate != NULL)
    {
        return g_MhVmic.PcCreate(DeviceObject, Irp);
    }
    return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS MhVmicDispatchClose(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    if (MhIsControlDevice(DeviceObject))
    {
        PIO_STACK_LOCATION Stack = IoGetCurrentIrpStackLocation(Irp);
        MhVmicReleaseWriter(Stack->FileObject);
        return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
    }
    if (g_MhVmic.PcClose != NULL)
    {
        return g_MhVmic.PcClose(DeviceObject, Irp);
    }
    return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS MhVmicDispatchCleanup(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    if (MhIsControlDevice(DeviceObject))
    {
        return MhVmicControlCleanup(Irp);
    }
    if (g_MhVmic.PcCleanup != NULL)
    {
        return g_MhVmic.PcCleanup(DeviceObject, Irp);
    }
    return MhVmicComplete(Irp, STATUS_SUCCESS, 0);
}

static NTSTATUS MhVmicDispatchDeviceControl(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    if (MhIsControlDevice(DeviceObject))
    {
        return MhVmicControlDeviceControl(Irp);
    }
    if (g_MhVmic.PcDeviceControl != NULL)
    {
        return g_MhVmic.PcDeviceControl(DeviceObject, Irp);
    }
    return MhVmicComplete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
}

static NTSTATUS MhVmicDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    NTSTATUS Status;
    BOOLEAN Remove;

    if (MhIsControlDevice(DeviceObject))
    {
        return MhVmicComplete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }

    Remove = IoGetCurrentIrpStackLocation(Irp)->MinorFunction == IRP_MN_REMOVE_DEVICE;
    Status = g_MhVmic.PcPnp != NULL
        ? g_MhVmic.PcPnp(DeviceObject, Irp)
        : MhVmicComplete(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);

    if (Remove)
    {
        MhVmicRemoveAdapterState();
    }
    return Status;
}

/* ------------------------------------------------------------------------- */
/* Adapter / subdevice setup                                                 */
/* ------------------------------------------------------------------------- */

static NTSTATUS MhVmicInstallSubdevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                                       _In_ PCWSTR Name, _In_ REFCLSID PortClassId,
                                       _In_ PUNKNOWN Miniport, _In_ PUNKNOWN Adapter,
                                       _Out_opt_ PUNKNOWN *OutPortUnknown)
{
    NTSTATUS Status;
    PPORT Port = NULL;

    if (OutPortUnknown != NULL)
    {
        *OutPortUnknown = NULL;
    }

    Status = PcNewPort(&Port, PortClassId);
    if (NT_SUCCESS(Status))
    {
        Status = Port->Init(DeviceObject, Irp, Miniport, Adapter, NULL);
    }
    if (NT_SUCCESS(Status))
    {
        Status = PcRegisterSubdevice(DeviceObject, (PWSTR)Name, Port);
    }
    if (NT_SUCCESS(Status) && OutPortUnknown != NULL)
    {
        Status = Port->QueryInterface(IID_IUnknown, (PVOID *)OutPortUnknown);
    }

    if (Port != NULL)
    {
        Port->Release();
    }
    return Status;
}

static NTSTATUS MhVmicStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp,
                                  _In_ PRESOURCELIST ResourceList)
{
    NTSTATUS Status;
    PUNKNOWN PowerUnknown = NULL;
    PUNKNOWN WaveMiniport = NULL;
    PUNKNOWN TopologyMiniport = NULL;
    PUNKNOWN WavePort = NULL;
    PUNKNOWN TopologyPort = NULL;

    UNREFERENCED_PARAMETER(ResourceList);
    PAGED_CODE();

    if (g_MhVmic.ControlDevice == NULL)
    {
        Status = MhVmicCreateControlDevice(DeviceObject->DriverObject);
        if (!NT_SUCCESS(Status))
        {
            return Status;
        }
    }

    KeWaitForSingleObject(&g_MhVmic.Mutex, Executive, KernelMode, FALSE, NULL);
    InterlockedExchange(&g_MhVmic.WriterActive, 0);
    g_MhVmic.WriterFile = NULL;
    KeReleaseMutex(&g_MhVmic.Mutex, FALSE);

    PowerUnknown = reinterpret_cast<PUNKNOWN>(new (POOL_FLAG_NON_PAGED, MH_VMIC_POOL_TAG) CAdapterContext(NULL));
    if (PowerUnknown == NULL)
    {
        Status = STATUS_INSUFFICIENT_RESOURCES;
        goto Exit;
    }
    PowerUnknown->AddRef();
    g_MhVmic.AdapterPower = PowerUnknown;

    Status = PcRegisterAdapterPowerManagement(PowerUnknown, DeviceObject);
    if (!NT_SUCCESS(Status))
    {
        goto Exit;
    }

    Status = MhVmicCreateWaveMiniport(&WaveMiniport, IID_IMiniportWaveRT, NULL,
        POOL_FLAG_NON_PAGED, PowerUnknown, NULL);
    if (!NT_SUCCESS(Status))
    {
        goto Exit;
    }

    Status = MhVmicCreateTopologyMiniport(&TopologyMiniport, IID_IMiniportTopology, NULL,
        POOL_FLAG_NON_PAGED, PowerUnknown, NULL);
    if (!NT_SUCCESS(Status))
    {
        goto Exit;
    }

    Status = MhVmicInstallSubdevice(DeviceObject, Irp, L"Wave", CLSID_PortWaveRT,
        WaveMiniport, PowerUnknown, &WavePort);
    if (!NT_SUCCESS(Status))
    {
        goto Exit;
    }

    Status = MhVmicInstallSubdevice(DeviceObject, Irp, L"Topology", CLSID_PortTopology,
        TopologyMiniport, PowerUnknown, &TopologyPort);
    if (!NT_SUCCESS(Status))
    {
        goto Exit;
    }

    Status = PcRegisterPhysicalConnection(DeviceObject, TopologyPort, 1, WavePort, 0);

Exit:
    MH_SAFE_RELEASE(WavePort);
    MH_SAFE_RELEASE(TopologyPort);
    MH_SAFE_RELEASE(WaveMiniport);
    MH_SAFE_RELEASE(TopologyMiniport);

    if (PowerUnknown != NULL)
    {
        PowerUnknown->Release();
    }

    if (!NT_SUCCESS(Status))
    {
        MhVmicRemoveAdapterState();
    }
    return Status;
}

static NTSTATUS MhVmicAddDevice(_In_ PDRIVER_OBJECT DriverObject, _In_ PDEVICE_OBJECT PhysicalDeviceObject)
{
    PAGED_CODE();
    return PcAddAdapterDevice(DriverObject, PhysicalDeviceObject,
        PCPFNSTARTDEVICE(MhVmicStartDevice), 4, 0);
}

static VOID MhVmicDriverUnload(_In_ PDRIVER_OBJECT DriverObject)
{
    MhVmicRemoveAdapterState();

    if (g_MhVmic.PcUnload != NULL)
    {
        PDRIVER_UNLOAD PcUnload = g_MhVmic.PcUnload;
        g_MhVmic.PcUnload = NULL;
        PcUnload(DriverObject);
    }
}

/* ------------------------------------------------------------------------- */
/* Driver entry                                                              */
/* ------------------------------------------------------------------------- */

extern "C"
NTSTATUS
DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    NTSTATUS Status;

    RtlZeroMemory(&g_MhVmic, sizeof(g_MhVmic));
    KeInitializeMutex(&g_MhVmic.Mutex, 0);
    g_MhVmic.SampleRate = MH_VMIC_DEFAULT_SAMPLE_RATE;
    g_MhVmic.Channels = MH_VMIC_DEFAULT_CHANNELS;
    g_MhVmic.PacketFrames = MH_VMIC_DEFAULT_PACKET_FRAMES;
    g_MhVmic.QpcFrequency = MhVmicQpcFrequency();
    MhGenerateSectionName();

    Status = PcInitializeAdapterDriver(DriverObject, RegistryPath,
        (PDRIVER_ADD_DEVICE)MhVmicAddDevice);
    if (!NT_SUCCESS(Status))
    {
        return Status;
    }

    g_MhVmic.PcCreate = DriverObject->MajorFunction[IRP_MJ_CREATE];
    g_MhVmic.PcClose = DriverObject->MajorFunction[IRP_MJ_CLOSE];
    g_MhVmic.PcCleanup = DriverObject->MajorFunction[IRP_MJ_CLEANUP];
    g_MhVmic.PcDeviceControl = DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL];
    g_MhVmic.PcPnp = DriverObject->MajorFunction[IRP_MJ_PNP];
    g_MhVmic.PcUnload = DriverObject->DriverUnload;

    DriverObject->MajorFunction[IRP_MJ_CREATE] = MhVmicDispatchCreate;
    DriverObject->MajorFunction[IRP_MJ_CLOSE] = MhVmicDispatchClose;
    DriverObject->MajorFunction[IRP_MJ_CLEANUP] = MhVmicDispatchCleanup;
    DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL] = MhVmicDispatchDeviceControl;
    DriverObject->MajorFunction[IRP_MJ_PNP] = MhVmicDispatchPnp;
    DriverObject->DriverUnload = MhVmicDriverUnload;

    g_MhVmic.Initialized = TRUE;
    return STATUS_SUCCESS;
}
