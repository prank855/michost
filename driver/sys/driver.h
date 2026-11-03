#ifndef MH_VMIC_DRIVER_H
#define MH_VMIC_DRIVER_H

#include <wdm.h>
#include <portcls.h>
#include <stdunk.h>
#include <ksmedia.h>
#include <ntstrsafe.h>
#include <wdmsec.h>
#include <bcrypt.h>

#include "mh_vmic.h"

#define MH_VMIC_POOL_TAG 'MVmH'
#define MH_VMIC_DEVICE_NAME_U L"\\Device\\MicHostVirtualMic"
#define MH_VMIC_SYMLINK_NAME_U L"\\DosDevices\\MicHostVirtualMic"
#define MH_VMIC_SESSION_OBJECTS_U L"\\Sessions\\%lu\\BaseNamedObjects\\%s"
#define MH_VMIC_CONTROL_SDDL L"D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)"

/* Kernel pool allocation for new/delete (defined in driver.cpp). */
void *__cdecl operator new(size_t Size);
void *__cdecl operator new(size_t Size, POOL_FLAGS Flags, ULONG Tag);
void __cdecl operator delete(void *Pointer);
void __cdecl operator delete(void *Pointer, size_t Size);

typedef struct MH_VMIC_STATE
{
    KMUTEX Mutex;
    BOOLEAN Initialized;

    HANDLE SectionHandle;
    PVOID SectionView;
    PMDL RingMdl;
    MH_VMIC_RING *Ring;
    ULONG RingFrames;
    volatile LONG PacketFrames;
    volatile LONG SampleRate;
    volatile LONG Channels;
    ULONG SectionBytes;
    BOOLEAN RingMdlLocked;

    PFILE_OBJECT WriterFile;
    volatile LONG WriterActive;
    volatile LONG ConsumerActive;
    volatile LONG StreamCount;
    ULONGLONG QpcFrequency;
    ULONG SessionId;

    UNICODE_STRING SectionName;
    WCHAR SectionNameBuffer[MH_VMIC_SECTION_NAME_CHARS];

    PDEVICE_OBJECT ControlDevice;
    BOOLEAN ControlSymbolicLinkCreated;

    PDRIVER_DISPATCH PcCreate;
    PDRIVER_DISPATCH PcClose;
    PDRIVER_DISPATCH PcCleanup;
    PDRIVER_DISPATCH PcDeviceControl;
    PDRIVER_DISPATCH PcPnp;
    PDRIVER_UNLOAD PcUnload;
    PUNKNOWN AdapterPower;
} MH_VMIC_STATE;

extern MH_VMIC_STATE g_MhVmic;

static __inline ULONGLONG MhVmicQpcFrequency(VOID)
{
    LARGE_INTEGER Frequency;
    (VOID)KeQueryPerformanceCounter(&Frequency);
    return (ULONGLONG)Frequency.QuadPart;
}

/* Ring/control helpers (driver.cpp). */
MH_VMIC_RING *MhVmicRingAcquire(VOID);
VOID MhVmicStreamOpened(VOID);
VOID MhVmicStreamClosed(VOID);
VOID MhVmicSetConsumerActive(BOOLEAN Active);
BOOLEAN MhVmicConsumerActive(VOID);
ULONG MhVmicCurrentSampleRate(VOID);
ULONG MhVmicCurrentChannels(VOID);
ULONG MhVmicCurrentPacketFrames(VOID);
VOID MhVmicReleaseWriter(PFILE_OBJECT FileObject);

/* Wave miniport factory/power. */
NTSTATUS MhVmicCreateWaveMiniport(_Out_ PUNKNOWN *Unknown, _In_ REFCLSID ClassId,
                                  _In_opt_ PUNKNOWN OuterUnknown, _In_ POOL_FLAGS PoolFlags,
                                  _In_opt_ PUNKNOWN Adapter, _In_opt_ PVOID Context);
VOID MhVmicWavePowerStop(VOID);

/* Topology miniport factory. */
NTSTATUS MhVmicCreateTopologyMiniport(_Out_ PUNKNOWN *Unknown, _In_ REFCLSID ClassId,
                                      _In_opt_ PUNKNOWN OuterUnknown, _In_ POOL_FLAGS PoolFlags,
                                      _In_opt_ PUNKNOWN Adapter, _In_opt_ PVOID Context);

#endif /* MH_VMIC_DRIVER_H */
