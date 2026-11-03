#ifndef MH_VMIC_MINWAVERT_H
#define MH_VMIC_MINWAVERT_H

#include "driver.h"

class CMiniportWaveRTStream;

EXT_CALLBACK MhVmicStreamTimer;

class CMiniportWaveRT : public IMiniportWaveRT, public CUnknown
{
public:
    CMiniportWaveRT(_In_opt_ PUNKNOWN OuterUnknown);
    ~CMiniportWaveRT();

    DECLARE_STD_UNKNOWN();
    IMP_IMiniportWaveRT;

    STDMETHODIMP_(NTSTATUS) GetDeviceDescription(_Out_ PDEVICE_DESCRIPTION DmaDeviceDescription);

    VOID OnPowerD3(VOID);
    VOID StreamCreated(_In_ CMiniportWaveRTStream *Stream);
    VOID StreamClosed(_In_ CMiniportWaveRTStream *Stream);

private:
    PUNKNOWN m_Adapter;
    CMiniportWaveRTStream *m_ActiveStream;
};

class CMiniportWaveRTStream
    : public IMiniportWaveRTStreamNotification
    , public IMiniportWaveRTInputStream
    , public CUnknown
{
public:
    CMiniportWaveRTStream(_In_opt_ PUNKNOWN OuterUnknown);
    ~CMiniportWaveRTStream();

    DECLARE_STD_UNKNOWN();
    IMP_IMiniportWaveRTStream;
    IMP_IMiniportWaveRTStreamNotification;
    IMP_IMiniportWaveRTInputStream;

    NTSTATUS Init(_In_ CMiniportWaveRT *Miniport,
                  _In_ PPORTWAVERTSTREAM PortStream,
                  _In_ ULONG Pin,
                  _In_ BOOLEAN Capture,
                  _In_ PKSDATAFORMAT DataFormat);

    VOID PowerStop(VOID);

private:
    friend EXT_CALLBACK MhVmicStreamTimer;
    VOID StopTimer(VOID);
    VOID WriteOnePacket(VOID);
    BOOLEAN ValidateFormat(_In_ const WAVEFORMATEX *Format) const;

    CMiniportWaveRT *m_Miniport;
    PPORTWAVERTSTREAM m_PortStream;
    ULONG m_Pin;
    BOOLEAN m_Capture;
    BOOLEAN m_Counted;

    BYTE *m_DmaBuffer;
    ULONG m_DmaBufferSize;
    ULONG m_NotificationCount;
    ULONG m_PacketBytes;
    ULONG m_NotificationFrames;
    ULONG m_FrameBytes;
    ULONG m_SampleRate;
    ULONG m_Channels;

    KSSTATE m_State;
    volatile LONG m_Running;
    PEX_TIMER m_Timer;
    LONGLONG m_QpcFrequency;
    LONGLONG m_QpcPerPacket;
    LONGLONG m_QpcNextPacket;
    volatile LONGLONG m_QpcLastPacket;
    ULONG m_WritePositionBytes;
    volatile LONG m_PacketsCompleted;
    ULONG m_LastOsReadPacket;

    PKEVENT m_NotificationEvents[4];
    volatile LONG m_NotificationEventCount;
};

#endif /* MH_VMIC_MINWAVERT_H */
