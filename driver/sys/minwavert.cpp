/*
 * MicHost virtual microphone - WaveRT capture miniport.
 */

#include "driver.h"
#include "minwavert.h"

#define KSNODE_WAVE_ADC 0
#define KSPIN_WAVE_BRIDGE 0
#define KSPIN_WAVE_CAPTURE 1

static CMiniportWaveRT *g_WaveMiniport = NULL;

static KSDATAFORMAT_WAVEFORMATEXTENSIBLE g_MicFormat =
{
    {
        sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE),
        0,
        0,
        0,
        STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
        STATICGUIDOF(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT),
        STATICGUIDOF(KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)
    },
    {
        WAVE_FORMAT_EXTENSIBLE,
        2,
        48000,
        384000,
        8,
        32,
        sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)
    },
    32,
    KSAUDIO_SPEAKER_STEREO,
    STATICGUIDOF(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
};

static KSDATAFORMAT g_BridgeRange =
{
    sizeof(KSDATAFORMAT),
    0,
    0,
    0,
    STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
    STATICGUIDOF(KSDATAFORMAT_SUBTYPE_ANALOG),
    STATICGUIDOF(KSDATAFORMAT_SPECIFIER_NONE)
};

static PKSDATARANGE g_CaptureRanges[] = { PKSDATARANGE(&g_MicFormat.DataFormat) };
static PKSDATARANGE g_BridgeRanges[] = { &g_BridgeRange };

static PCPIN_DESCRIPTOR g_WavePins[] =
{
    {
        0, 0, 0, NULL,
        {
            0, NULL, 0, NULL,
            SIZEOF_ARRAY(g_BridgeRanges), g_BridgeRanges,
            KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE,
            &KSCATEGORY_AUDIO, NULL, 0
        }
    },
    {
        1, 1, 0, NULL,
        {
            0, NULL, 0, NULL,
            SIZEOF_ARRAY(g_CaptureRanges), g_CaptureRanges,
            KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_SINK,
            &KSCATEGORY_AUDIO, &KSAUDFNAME_RECORDING_CONTROL, 0
        }
    }
};

static PCNODE_DESCRIPTOR g_WaveNodes[] =
{
    { 0, NULL, &KSNODETYPE_ADC, NULL }
};

static PCCONNECTION_DESCRIPTOR g_WaveConnections[] =
{
    { PCFILTER_NODE, KSPIN_WAVE_BRIDGE, KSNODE_WAVE_ADC, 1 },
    { KSNODE_WAVE_ADC, 0, PCFILTER_NODE, KSPIN_WAVE_CAPTURE }
};

static PCFILTER_DESCRIPTOR g_WaveFilterDescriptor =
{
    0,
    NULL,
    sizeof(PCPIN_DESCRIPTOR),
    SIZEOF_ARRAY(g_WavePins),
    g_WavePins,
    sizeof(PCNODE_DESCRIPTOR),
    SIZEOF_ARRAY(g_WaveNodes),
    g_WaveNodes,
    SIZEOF_ARRAY(g_WaveConnections),
    g_WaveConnections,
    0,
    NULL
};

/* ------------------------------------------------------------------------- */
/* Factory                                                                   */
/* ------------------------------------------------------------------------- */

NTSTATUS MhVmicCreateWaveMiniport(_Out_ PUNKNOWN *Unknown, _In_ REFCLSID ClassId,
                                  _In_opt_ PUNKNOWN OuterUnknown, _In_ POOL_FLAGS PoolFlags,
                                  _In_opt_ PUNKNOWN Adapter, _In_opt_ PVOID Context)
{
    CMiniportWaveRT *Object;

    UNREFERENCED_PARAMETER(ClassId);
    UNREFERENCED_PARAMETER(Adapter);
    UNREFERENCED_PARAMETER(Context);

    if (Unknown == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    Object = new (PoolFlags, MH_VMIC_POOL_TAG) CMiniportWaveRT(OuterUnknown);
    if (Object == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Object->AddRef();
    *Unknown = reinterpret_cast<PUNKNOWN>(Object);
    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------- */
/* Wave miniport                                                             */
/* ------------------------------------------------------------------------- */

CMiniportWaveRT::CMiniportWaveRT(_In_opt_ PUNKNOWN OuterUnknown)
    : CUnknown(OuterUnknown),
      m_Adapter(NULL),
      m_ActiveStream(NULL)
{
}

CMiniportWaveRT::~CMiniportWaveRT()
{
    if (g_WaveMiniport == this)
    {
        g_WaveMiniport = NULL;
    }
    if (m_Adapter != NULL)
    {
        m_Adapter->Release();
        m_Adapter = NULL;
    }
}

STDMETHODIMP
CMiniportWaveRT::NonDelegatingQueryInterface(_In_ REFIID Interface, _Out_ PVOID *Object)
{
    PAGED_CODE();

    if (Object == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    if (IsEqualGUIDAligned(Interface, IID_IUnknown))
    {
        *Object = PVOID(PUNKNOWN(PMINIPORTWAVERT(this)));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniport))
    {
        *Object = PVOID(PMINIPORT(this));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRT))
    {
        *Object = PVOID(PMINIPORTWAVERT(this));
    }
    else
    {
        return STATUS_INVALID_PARAMETER;
    }

    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRT::Init(_In_ PUNKNOWN UnknownAdapter, _In_ PRESOURCELIST ResourceList,
                      _In_ PPORTWAVERT Port)
{
    UNREFERENCED_PARAMETER(ResourceList);
    PAGED_CODE();

    if (Port == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    m_Adapter = UnknownAdapter;
    if (m_Adapter != NULL)
    {
        m_Adapter->AddRef();
    }
    g_WaveMiniport = this;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRT::GetDescription(_Out_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor)
{
    PAGED_CODE();

    if (OutFilterDescriptor == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    *OutFilterDescriptor = &g_WaveFilterDescriptor;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRT::GetDeviceDescription(_Out_ PDEVICE_DESCRIPTION DmaDeviceDescription)
{
    PAGED_CODE();

    if (DmaDeviceDescription == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    RtlZeroMemory(DmaDeviceDescription, sizeof(*DmaDeviceDescription));
    DmaDeviceDescription->Master = TRUE;
    DmaDeviceDescription->ScatterGather = TRUE;
    DmaDeviceDescription->Dma32BitAddresses = TRUE;
    DmaDeviceDescription->InterfaceType = PCIBus;
    DmaDeviceDescription->MaximumLength = 0xFFFFFFFF;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRT::DataRangeIntersection(_In_ ULONG PinId,
                                       _In_ PKSDATARANGE ClientDataRange,
                                       _In_ PKSDATARANGE MyDataRange,
                                       _In_ ULONG OutputBufferLength,
                                       _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength)
                                            PVOID ResultantFormat,
                                       _Out_ PULONG ResultantFormatLength)
{
    ULONG RequiredSize = sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE);

    UNREFERENCED_PARAMETER(MyDataRange);
    PAGED_CODE();

    if (ResultantFormatLength == NULL || ClientDataRange == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (PinId != KSPIN_WAVE_CAPTURE)
    {
        return STATUS_NOT_IMPLEMENTED;
    }
    if (!IsEqualGUIDAligned(ClientDataRange->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX))
    {
        return STATUS_NOT_IMPLEMENTED;
    }

    if (OutputBufferLength == 0)
    {
        *ResultantFormatLength = RequiredSize;
        return STATUS_BUFFER_OVERFLOW;
    }
    if (OutputBufferLength < RequiredSize)
    {
        return STATUS_BUFFER_TOO_SMALL;
    }

    if (ResultantFormat != NULL)
    {
        RtlCopyMemory(ResultantFormat, &g_MicFormat, sizeof(g_MicFormat));
    }
    *ResultantFormatLength = RequiredSize;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRT::NewStream(_Out_ PMINIPORTWAVERTSTREAM *OutStream,
                           _In_ PPORTWAVERTSTREAM OuterUnknown,
                           _In_ ULONG Pin,
                           _In_ BOOLEAN Capture,
                           _In_ PKSDATAFORMAT DataFormat)
{
    NTSTATUS Status;
    CMiniportWaveRTStream *Stream;

    PAGED_CODE();

    if (OutStream == NULL || DataFormat == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    *OutStream = NULL;

    if (!Capture || Pin != KSPIN_WAVE_CAPTURE)
    {
        return STATUS_NOT_SUPPORTED;
    }
    if (m_ActiveStream != NULL)
    {
        return STATUS_DEVICE_BUSY;
    }

    Stream = new (POOL_FLAG_NON_PAGED, MH_VMIC_POOL_TAG) CMiniportWaveRTStream(NULL);
    if (Stream == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Stream->AddRef();
    Status = Stream->Init(this, OuterUnknown, Pin, Capture, DataFormat);
    if (NT_SUCCESS(Status))
    {
        StreamCreated(Stream);
        *OutStream = PMINIPORTWAVERTSTREAM(Stream);
        (*OutStream)->AddRef();
    }
    Stream->Release();
    return Status;
}

VOID CMiniportWaveRT::StreamCreated(_In_ CMiniportWaveRTStream *Stream)
{
    m_ActiveStream = Stream;
}

VOID CMiniportWaveRT::StreamClosed(_In_ CMiniportWaveRTStream *Stream)
{
    if (m_ActiveStream == Stream)
    {
        m_ActiveStream = NULL;
    }
}

VOID CMiniportWaveRT::OnPowerD3(VOID)
{
    if (m_ActiveStream != NULL)
    {
        m_ActiveStream->PowerStop();
    }
}

VOID MhVmicWavePowerStop(VOID)
{
    CMiniportWaveRT *Miniport = g_WaveMiniport;
    if (Miniport != NULL)
    {
        Miniport->OnPowerD3();
    }
}

/* ------------------------------------------------------------------------- */
/* Stream                                                                    */
/* ------------------------------------------------------------------------- */

CMiniportWaveRTStream::CMiniportWaveRTStream(_In_opt_ PUNKNOWN OuterUnknown)
    : CUnknown(OuterUnknown),
      m_Miniport(NULL),
      m_PortStream(NULL),
      m_Pin(0),
      m_Capture(FALSE),
      m_Counted(FALSE),
      m_DmaBuffer(NULL),
      m_DmaBufferSize(0),
      m_NotificationCount(0),
      m_PacketBytes(0),
      m_NotificationFrames(0),
      m_FrameBytes(0),
      m_SampleRate(0),
      m_Channels(0),
      m_State(KSSTATE_STOP),
      m_Running(0),
      m_Timer(NULL),
      m_QpcFrequency(0),
      m_QpcPerPacket(0),
      m_QpcNextPacket(0),
      m_QpcLastPacket(0),
      m_WritePositionBytes(0),
      m_PacketsCompleted(0),
      m_LastOsReadPacket(MAXULONG),
      m_NotificationEventCount(0)
{
    RtlZeroMemory(m_NotificationEvents, sizeof(m_NotificationEvents));
}

CMiniportWaveRTStream::~CMiniportWaveRTStream()
{
    StopTimer();
    if (m_Timer != NULL)
    {
        ExDeleteTimer(m_Timer, TRUE, TRUE, NULL);
        m_Timer = NULL;
    }
    if (m_Counted)
    {
        MhVmicStreamClosed();
    }
    if (m_Miniport != NULL)
    {
        m_Miniport->StreamClosed(this);
        m_Miniport = NULL;
    }
    m_PortStream = NULL;
}

STDMETHODIMP
CMiniportWaveRTStream::NonDelegatingQueryInterface(_In_ REFIID Interface, _Out_ PVOID *Object)
{
    PAGED_CODE();

    if (Object == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    if (IsEqualGUIDAligned(Interface, IID_IUnknown))
    {
        *Object = PVOID(PUNKNOWN(PMINIPORTWAVERTSTREAM(this)));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTStream))
    {
        *Object = PVOID(PMINIPORTWAVERTSTREAM(this));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTStreamNotification))
    {
        *Object = PVOID(PMINIPORTWAVERTSTREAMNOTIFICATION(this));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniportWaveRTInputStream) && m_Capture)
    {
        *Object = PVOID(PMINIPORTWAVERTINPUTSTREAM(this));
    }
    else
    {
        return STATUS_INVALID_PARAMETER;
    }

    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

NTSTATUS CMiniportWaveRTStream::Init(_In_ CMiniportWaveRT *Miniport,
                                     _In_ PPORTWAVERTSTREAM PortStream,
                                     _In_ ULONG Pin,
                                     _In_ BOOLEAN Capture,
                                     _In_ PKSDATAFORMAT DataFormat)
{
    PAGED_CODE();

    if (Miniport == NULL || PortStream == NULL || DataFormat == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (DataFormat->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEX) ||
        !IsEqualGUIDAligned(DataFormat->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX))
    {
        return STATUS_NOT_SUPPORTED;
    }

    {
        const WAVEFORMATEX *Format =
            (const WAVEFORMATEX *)((const BYTE *)DataFormat + sizeof(KSDATAFORMAT));
        if (!ValidateFormat(Format))
        {
            return STATUS_NOT_SUPPORTED;
        }
        m_FrameBytes = Format->nBlockAlign;
        m_SampleRate = Format->nSamplesPerSec;
        m_Channels = Format->nChannels;
    }

    m_Miniport = Miniport;
    m_PortStream = PortStream;
    m_Pin = Pin;
    m_Capture = Capture;
    m_QpcFrequency = (LONGLONG)MhVmicQpcFrequency();

    m_Timer = ExAllocateTimer(MhVmicStreamTimer, this, EX_TIMER_HIGH_RESOLUTION);
    if (m_Timer == NULL)
    {
        m_Miniport = NULL;
        m_PortStream = NULL;
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    MhVmicStreamOpened();
    m_Counted = TRUE;
    return STATUS_SUCCESS;
}

BOOLEAN CMiniportWaveRTStream::ValidateFormat(_In_ const WAVEFORMATEX *Format) const
{
    if (Format == NULL)
    {
        return FALSE;
    }
    if (Format->wFormatTag != WAVE_FORMAT_EXTENSIBLE &&
        Format->wFormatTag != WAVE_FORMAT_IEEE_FLOAT)
    {
        return FALSE;
    }
    if (Format->nChannels < 1 || Format->nChannels > 2)
    {
        return FALSE;
    }
    if (Format->nSamplesPerSec != 48000)
    {
        return FALSE;
    }
    if (Format->wBitsPerSample != 32)
    {
        return FALSE;
    }
    if (Format->nBlockAlign != Format->nChannels * sizeof(float))
    {
        return FALSE;
    }

    /* The writer must agree with the endpoint format. */
    if (MhVmicCurrentSampleRate() != 0 &&
        (MhVmicCurrentSampleRate() != Format->nSamplesPerSec ||
         MhVmicCurrentChannels() != Format->nChannels))
    {
        return FALSE;
    }
    return TRUE;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::SetFormat(_In_ PKSDATAFORMAT DataFormat)
{
    PAGED_CODE();

    if (DataFormat == NULL ||
        DataFormat->FormatSize < sizeof(KSDATAFORMAT_WAVEFORMATEX) ||
        !IsEqualGUIDAligned(DataFormat->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX))
    {
        return STATUS_NOT_SUPPORTED;
    }

    {
        const WAVEFORMATEX *Format =
            (const WAVEFORMATEX *)((const BYTE *)DataFormat + sizeof(KSDATAFORMAT));
        if (!ValidateFormat(Format))
        {
            return STATUS_NOT_SUPPORTED;
        }
        m_FrameBytes = Format->nBlockAlign;
        m_SampleRate = Format->nSamplesPerSec;
        m_Channels = Format->nChannels;
    }
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::SetState(_In_ KSSTATE State)
{
    NTSTATUS Status = STATUS_SUCCESS;

    PAGED_CODE();

    switch (State)
    {
    case KSSTATE_STOP:
        StopTimer();
        if (m_DmaBuffer != NULL && m_DmaBufferSize != 0)
        {
            RtlZeroMemory(m_DmaBuffer, m_DmaBufferSize);
        }
        m_WritePositionBytes = 0;
        InterlockedExchange(&m_PacketsCompleted, 0);
        InterlockedExchange64((volatile LONG64 *)&m_QpcLastPacket, 0);
        m_LastOsReadPacket = MAXULONG;
        break;

    case KSSTATE_ACQUIRE:
        break;

    case KSSTATE_PAUSE:
        StopTimer();
        break;

    case KSSTATE_RUN:
        if (m_DmaBuffer == NULL || m_DmaBufferSize == 0 ||
            m_NotificationCount == 0 || m_NotificationFrames == 0)
        {
            return STATUS_INVALID_DEVICE_STATE;
        }
        if (InterlockedCompareExchange(&m_Running, 0, 0) != 0)
        {
            break;
        }
        {
            LARGE_INTEGER Qpc = KeQueryPerformanceCounter(NULL);
            LONGLONG Period = ((LONGLONG)m_NotificationFrames * 10000000LL) / (LONGLONG)m_SampleRate;
            if (Period < 10000)
            {
                Period = 10000;
            }

            m_QpcFrequency = (LONGLONG)MhVmicQpcFrequency();
            if (m_QpcFrequency <= 0)
            {
                m_QpcFrequency = 1;
            }
            m_QpcPerPacket = ((LONGLONG)m_NotificationFrames * m_QpcFrequency) /
                (LONGLONG)m_SampleRate;
            m_QpcNextPacket = Qpc.QuadPart;
            InterlockedExchange64((volatile LONG64 *)&m_QpcLastPacket, 0);
            m_WritePositionBytes = 0;
            InterlockedExchange(&m_PacketsCompleted, 0);
            m_LastOsReadPacket = MAXULONG;
            RtlZeroMemory(m_DmaBuffer, m_DmaBufferSize);
            m_State = KSSTATE_RUN;
            InterlockedExchange(&m_Running, 1);
            MhVmicSetConsumerActive(TRUE);
            if (!ExSetTimer(m_Timer, -Period, Period, NULL))
            {
                InterlockedExchange(&m_Running, 0);
                MhVmicSetConsumerActive(FALSE);
                Status = STATUS_UNSUCCESSFUL;
            }
        }
        break;
    }

    m_State = State;
    return Status;
}

VOID CMiniportWaveRTStream::StopTimer(VOID)
{
    InterlockedExchange(&m_Running, 0);
    KeMemoryBarrier();
    if (m_Timer != NULL)
    {
        ExCancelTimer(m_Timer, NULL);
    }
    KeFlushQueuedDpcs();
    MhVmicSetConsumerActive(FALSE);
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::GetPosition(_Out_ PKSAUDIO_POSITION Position)
{
    ULONG Offset = 0;

    if (Position == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (m_DmaBufferSize != 0)
    {
        Offset = m_WritePositionBytes % m_DmaBufferSize;
    }
    Position->PlayOffset = Offset;
    Position->WriteOffset = Offset;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(VOID)
CMiniportWaveRTStream::GetHWLatency(_Out_ PKSRTAUDIO_HWLATENCY Latency)
{
    if (Latency == NULL)
    {
        return;
    }
    ULONG FifoSize = m_PacketBytes;
    ULONG RingFrames = g_MhVmic.RingFrames;
    ULONG RingChannels = (ULONG)g_MhVmic.Channels;
    MH_VMIC_RING *Ring = MhVmicRingAcquire();

    if (Ring != NULL && RingFrames != 0 && RingChannels != 0 && RingChannels <= 2)
    {
        ULONG64 Write = (ULONG64)InterlockedCompareExchange64(
            (volatile LONG64 *)&Ring->write_index, 0, 0);
        ULONG64 Read = (ULONG64)InterlockedCompareExchange64(
            (volatile LONG64 *)&Ring->read_index, 0, 0);
        ULONG64 Buffered = Write > Read ? Write - Read : 0;
        if (Buffered > RingFrames)
        {
            Buffered = RingFrames;
        }
        FifoSize += (ULONG)(Buffered * RingChannels * sizeof(float));
    }

    Latency->ChipsetDelay = 0;
    Latency->CodecDelay = 0;
    Latency->FifoSize = FifoSize;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::GetClockRegister(_Out_ PKSRTAUDIO_HWREGISTER Register)
{
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_IMPLEMENTED;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::GetPositionRegister(_Out_ PKSRTAUDIO_HWREGISTER Register)
{
    UNREFERENCED_PARAMETER(Register);
    return STATUS_NOT_IMPLEMENTED;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::AllocateAudioBuffer(_In_ ULONG RequestedSize,
                                           _Out_ PMDL *AudioBufferMdl,
                                           _Out_ ULONG *ActualSize,
                                           _Out_ ULONG *OffsetFromFirstPage,
                                           _Out_ MEMORY_CACHING_TYPE *CacheType)
{
    return AllocateBufferWithNotification(1, RequestedSize, AudioBufferMdl,
        ActualSize, OffsetFromFirstPage, CacheType);
}

STDMETHODIMP_(VOID)
CMiniportWaveRTStream::FreeAudioBuffer(_In_opt_ PMDL Mdl, _In_ ULONG Size)
{
    FreeBufferWithNotification(Mdl, Size);
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::AllocateBufferWithNotification(
    _In_ ULONG NotificationCount,
    _In_ ULONG RequestedSize,
    _Out_ PMDL *AudioBufferMdl,
    _Out_ ULONG *ActualSize,
    _Out_ ULONG *OffsetFromFirstPage,
    _Out_ MEMORY_CACHING_TYPE *CacheType)
{
    PMDL Mdl;
    BYTE *DmaBuffer;
    PHYSICAL_ADDRESS HighAddress;

    PAGED_CODE();

    if (AudioBufferMdl == NULL || ActualSize == NULL ||
        OffsetFromFirstPage == NULL || CacheType == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (NotificationCount == 0 || NotificationCount > 4 ||
        RequestedSize < m_FrameBytes || m_FrameBytes == 0)
    {
        return STATUS_INVALID_PARAMETER;
    }

    RequestedSize -= RequestedSize % m_FrameBytes;
    if (RequestedSize / NotificationCount < m_FrameBytes)
    {
        return STATUS_INVALID_PARAMETER;
    }

    {
        ULONG NotificationFrames = (RequestedSize / NotificationCount) / m_FrameBytes;
        ULONG RingFrames = g_MhVmic.RingFrames;

        if (NotificationFrames == 0)
        {
            return STATUS_INVALID_PARAMETER;
        }
        if (RingFrames != 0 &&
            (NotificationFrames > RingFrames / 2 || NotificationFrames > MH_VMIC_MAX_PACKET_FRAMES))
        {
            return STATUS_INVALID_PARAMETER;
        }
    }

    HighAddress.QuadPart = MAXLONGLONG;
    Mdl = m_PortStream->AllocatePagesForMdl(HighAddress, RequestedSize);
    if (Mdl == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    DmaBuffer = (BYTE *)m_PortStream->MapAllocatedPages(Mdl, MmCached);
    if (DmaBuffer == NULL)
    {
        m_PortStream->FreePagesFromMdl(Mdl);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    m_DmaBuffer = DmaBuffer;
    m_DmaBufferSize = RequestedSize;
    m_NotificationCount = NotificationCount;
    m_PacketBytes = RequestedSize / NotificationCount;
    m_NotificationFrames = m_PacketBytes / m_FrameBytes;

    /* Publish the actual notification packet so the host pump targets the
       amount the driver really reads. */
    InterlockedExchange(&g_MhVmic.PacketFrames, (LONG)m_NotificationFrames);
    {
        MH_VMIC_RING *Ring = MhVmicRingAcquire();
        if (Ring != NULL)
        {
            InterlockedExchange((volatile LONG *)&Ring->packet_frames,
                (LONG)m_NotificationFrames);
        }
    }

    *AudioBufferMdl = Mdl;
    *ActualSize = RequestedSize;
    *OffsetFromFirstPage = 0;
    *CacheType = MmCached;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(VOID)
CMiniportWaveRTStream::FreeBufferWithNotification(_In_opt_ PMDL Mdl, _In_ ULONG Size)
{
    UNREFERENCED_PARAMETER(Size);

    if (Mdl != NULL)
    {
        if (m_DmaBuffer != NULL)
        {
            m_PortStream->UnmapAllocatedPages(m_DmaBuffer, Mdl);
        }
        m_PortStream->FreePagesFromMdl(Mdl);
    }
    m_DmaBuffer = NULL;
    m_DmaBufferSize = 0;
    m_NotificationCount = 0;
    m_PacketBytes = 0;
    m_NotificationFrames = 0;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::RegisterNotificationEvent(_In_ PKEVENT NotificationEvent)
{
    LONG Index;

    if (NotificationEvent == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    for (Index = 0; Index < RTL_NUMBER_OF(m_NotificationEvents); ++Index)
    {
        PKEVENT Existing = (PKEVENT)InterlockedCompareExchangePointer(
            (PVOID volatile *)&m_NotificationEvents[Index], NULL, NULL);
        if (Existing == NotificationEvent)
        {
            return STATUS_SUCCESS;
        }
    }

    for (Index = 0; Index < RTL_NUMBER_OF(m_NotificationEvents); ++Index)
    {
        if (InterlockedCompareExchangePointer(
                (PVOID volatile *)&m_NotificationEvents[Index], NotificationEvent, NULL) == NULL)
        {
            InterlockedIncrement(&m_NotificationEventCount);
            return STATUS_SUCCESS;
        }
    }

    return STATUS_INSUFFICIENT_RESOURCES;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::UnregisterNotificationEvent(_In_ PKEVENT NotificationEvent)
{
    LONG Index;

    for (Index = 0; Index < RTL_NUMBER_OF(m_NotificationEvents); ++Index)
    {
        if (InterlockedCompareExchangePointer(
                (PVOID volatile *)&m_NotificationEvents[Index], NULL, NotificationEvent) == NotificationEvent)
        {
            InterlockedDecrement(&m_NotificationEventCount);
            return STATUS_SUCCESS;
        }
    }
    return STATUS_NOT_FOUND;
}

STDMETHODIMP_(NTSTATUS)
CMiniportWaveRTStream::GetReadPacket(_Out_ PULONG PacketNumber,
                                     _Out_ PULONG Flags,
                                     _Out_ PULONG64 PerformanceCounterValue,
                                     _Out_ PBOOLEAN MoreData)
{
    ULONG Completed;
    ULONG Available;

    if (PacketNumber == NULL || Flags == NULL ||
        PerformanceCounterValue == NULL || MoreData == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    if (m_NotificationCount == 0)
    {
        return STATUS_NOT_SUPPORTED;
    }
    if (m_State < KSSTATE_PAUSE)
    {
        return STATUS_INVALID_DEVICE_STATE;
    }

    Completed = (ULONG)InterlockedCompareExchange(&m_PacketsCompleted, 0, 0);
    if (Completed == 0)
    {
        return STATUS_DEVICE_NOT_READY;
    }
    Available = Completed - 1;
    if (Available == m_LastOsReadPacket)
    {
        return STATUS_DEVICE_NOT_READY;
    }

    *Flags = 0;
    *PacketNumber = Available;
    *PerformanceCounterValue = (ULONG64)InterlockedCompareExchange64(
        (volatile LONG64 *)&m_QpcLastPacket, 0, 0);
    *MoreData = (m_LastOsReadPacket != MAXULONG &&
                 (Available - m_LastOsReadPacket) > 1) ? TRUE : FALSE;
    m_LastOsReadPacket = Available;
    return STATUS_SUCCESS;
}

VOID CMiniportWaveRTStream::PowerStop(VOID)
{
    StopTimer();
    if (m_State == KSSTATE_RUN)
    {
        m_State = KSSTATE_PAUSE;
    }
}

VOID CMiniportWaveRTStream::WriteOnePacket(VOID)
{
    ULONG Slot;
    BYTE *Destination;
    MH_VMIC_RING *Ring;
    ULONG ToCopy = 0;
    ULONG64 WriteIndex = 0;
    ULONG64 ReadIndex = 0;
    ULONG64 Available = 0;
    ULONG RingFrames = g_MhVmic.RingFrames;
    ULONG RingChannels = (ULONG)g_MhVmic.Channels;

    if (m_DmaBuffer == NULL || m_DmaBufferSize == 0 ||
        m_PacketBytes == 0 || m_NotificationCount == 0 ||
        m_NotificationFrames == 0)
    {
        return;
    }

    Slot = (ULONG)m_PacketsCompleted % m_NotificationCount;
    Destination = m_DmaBuffer + Slot * m_PacketBytes;
    RtlZeroMemory(Destination, m_PacketBytes);

    Ring = MhVmicRingAcquire();
    if (Ring != NULL &&
        Ring->magic == MH_VMIC_MAGIC &&
        Ring->version == MH_VMIC_VERSION &&
        Ring->bytes_per_sample == MH_VMIC_BYTES_PER_SAMPLE &&
        Ring->sample_rate == m_SampleRate &&
        Ring->channels == m_Channels &&
        RingChannels != 0 && RingChannels <= MH_VMIC_DEFAULT_CHANNELS &&
        RingFrames != 0 && RingFrames <= MH_VMIC_MAX_RING_FRAMES &&
        Ring->ring_frames == RingFrames &&
        Ring->channels == RingChannels &&
        InterlockedCompareExchange((volatile LONG *)&Ring->writer_active, 0, 0) != 0)
    {
        WriteIndex = (ULONG64)InterlockedCompareExchange64(
            (volatile LONG64 *)&Ring->write_index, 0, 0);
        ReadIndex = (ULONG64)InterlockedCompareExchange64(
            (volatile LONG64 *)&Ring->read_index, 0, 0);

        if (WriteIndex > ReadIndex)
        {
            Available = WriteIndex - ReadIndex;
        }
        if (Available > RingFrames)
        {
            Available = RingFrames;
        }
        if (Available > m_NotificationFrames)
        {
            ToCopy = m_NotificationFrames;
        }
        else
        {
            ToCopy = (ULONG)Available;
        }

        if (ToCopy != 0)
        {
            ULONG StartFrame = (ULONG)(ReadIndex % RingFrames);
            ULONG FirstFrames = RingFrames - StartFrame;
            ULONG Channels = RingChannels;
            const BYTE *Source = (const BYTE *)MH_VMIC_RING_DATA(Ring);

            if (FirstFrames > ToCopy)
            {
                FirstFrames = ToCopy;
            }
            RtlCopyMemory(Destination, Source + (SIZE_T)StartFrame * Channels * sizeof(float),
                (SIZE_T)FirstFrames * Channels * sizeof(float));
            if (ToCopy > FirstFrames)
            {
                RtlCopyMemory(Destination + (SIZE_T)FirstFrames * Channels * sizeof(float),
                    Source, (SIZE_T)(ToCopy - FirstFrames) * Channels * sizeof(float));
            }

            InterlockedExchange64((volatile LONG64 *)&Ring->read_index,
                (LONG64)(ReadIndex + ToCopy));
        }

        if (ToCopy < m_NotificationFrames)
        {
            InterlockedIncrement64((volatile LONG64 *)&Ring->underruns);
        }
    }
    else if (Ring != NULL &&
             Ring->magic == MH_VMIC_MAGIC &&
             InterlockedCompareExchange((volatile LONG *)&Ring->writer_active, 0, 0) != 0)
    {
        /* Format mismatch: the endpoint stays alive and silent. */
        InterlockedIncrement64((volatile LONG64 *)&Ring->underruns);
    }

    m_WritePositionBytes += m_PacketBytes;
    if (m_WritePositionBytes >= m_DmaBufferSize)
    {
        m_WritePositionBytes %= m_DmaBufferSize;
    }

    InterlockedExchange64((volatile LONG64 *)&m_QpcLastPacket, (LONGLONG)m_QpcNextPacket);
    m_QpcNextPacket += m_QpcPerPacket;
    KeMemoryBarrier();
    InterlockedIncrement(&m_PacketsCompleted);

    {
        LONG Count = InterlockedCompareExchange(&m_NotificationEventCount, 0, 0);
        LONG Index;
        if (Count > (LONG)RTL_NUMBER_OF(m_NotificationEvents))
        {
            Count = (LONG)RTL_NUMBER_OF(m_NotificationEvents);
        }
        for (Index = 0; Index < Count; ++Index)
        {
            PKEVENT Event = (PKEVENT)InterlockedCompareExchangePointer(
                (PVOID volatile *)&m_NotificationEvents[Index], NULL, NULL);
            if (Event != NULL)
            {
                KeSetEvent(Event, IO_NO_INCREMENT, FALSE);
            }
        }
    }
}

VOID
MhVmicStreamTimer(_In_ PEX_TIMER Timer, _In_opt_ PVOID DeferredContext)
{
    CMiniportWaveRTStream *Stream = (CMiniportWaveRTStream *)DeferredContext;

    UNREFERENCED_PARAMETER(Timer);

    if (Stream == NULL)
    {
        return;
    }
    if (InterlockedCompareExchange(&Stream->m_Running, 0, 0) == 0)
    {
        return;
    }
    Stream->WriteOnePacket();
}
