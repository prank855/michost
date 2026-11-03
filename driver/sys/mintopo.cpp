/*
 * MicHost virtual microphone - topology miniport.
 *
 * One integrated microphone pin and one bridge pin feeding the Wave filter.
 */

#include "driver.h"
#include "mintopo.h"

#define KSPIN_TOPO_MIC 0
#define KSPIN_TOPO_BRIDGE 1
#define KSNODE_TOPO_MIC 0

static KSDATAFORMAT g_TopoBridgeRange =
{
    sizeof(KSDATAFORMAT),
    0,
    0,
    0,
    STATICGUIDOF(KSDATAFORMAT_TYPE_AUDIO),
    STATICGUIDOF(KSDATAFORMAT_SUBTYPE_ANALOG),
    STATICGUIDOF(KSDATAFORMAT_SPECIFIER_NONE)
};

static PKSDATARANGE g_TopoBridgeRanges[] = { &g_TopoBridgeRange };

static PCPIN_DESCRIPTOR g_TopoPins[] =
{
    {
        0, 0, 0, NULL,
        {
            0, NULL, 0, NULL,
            SIZEOF_ARRAY(g_TopoBridgeRanges), g_TopoBridgeRanges,
            KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE,
            &KSNODETYPE_MICROPHONE, NULL, 0
        }
    },
    {
        0, 0, 0, NULL,
        {
            0, NULL, 0, NULL,
            SIZEOF_ARRAY(g_TopoBridgeRanges), g_TopoBridgeRanges,
            KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_NONE,
            &KSCATEGORY_AUDIO, NULL, 0
        }
    }
};

static PCNODE_DESCRIPTOR g_TopoNodes[] =
{
    { 0, NULL, &KSNODETYPE_MICROPHONE, NULL }
};

static PCCONNECTION_DESCRIPTOR g_TopoConnections[] =
{
    { PCFILTER_NODE, KSPIN_TOPO_MIC, KSNODE_TOPO_MIC, 1 },
    { KSNODE_TOPO_MIC, 0, PCFILTER_NODE, KSPIN_TOPO_BRIDGE }
};

static NTSTATUS MhVmicTopologyPropertyHandler(_In_ PPCPROPERTY_REQUEST PropertyRequest);

static PCPROPERTY_ITEM g_TopoProperties[] =
{
    {
        &KSPROPSETID_Jack,
        KSPROPERTY_JACK_DESCRIPTION,
        KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT,
        MhVmicTopologyPropertyHandler
    },
    {
        &KSPROPSETID_Jack,
        KSPROPERTY_JACK_DESCRIPTION2,
        KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT,
        MhVmicTopologyPropertyHandler
    }
};

DEFINE_PCAUTOMATION_TABLE_PROP(g_TopoAutomation, g_TopoProperties);

static PCFILTER_DESCRIPTOR g_TopoFilterDescriptor =
{
    0,
    &g_TopoAutomation,
    sizeof(PCPIN_DESCRIPTOR),
    SIZEOF_ARRAY(g_TopoPins),
    g_TopoPins,
    sizeof(PCNODE_DESCRIPTOR),
    SIZEOF_ARRAY(g_TopoNodes),
    g_TopoNodes,
    SIZEOF_ARRAY(g_TopoConnections),
    g_TopoConnections,
    0,
    NULL
};

/* ------------------------------------------------------------------------- */
/* Factory                                                                   */
/* ------------------------------------------------------------------------- */

NTSTATUS MhVmicCreateTopologyMiniport(_Out_ PUNKNOWN *Unknown, _In_ REFCLSID ClassId,
                                      _In_opt_ PUNKNOWN OuterUnknown, _In_ POOL_FLAGS PoolFlags,
                                      _In_opt_ PUNKNOWN Adapter, _In_opt_ PVOID Context)
{
    CMiniportTopology *Object;

    UNREFERENCED_PARAMETER(ClassId);
    UNREFERENCED_PARAMETER(Adapter);
    UNREFERENCED_PARAMETER(Context);

    if (Unknown == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    Object = new (PoolFlags, MH_VMIC_POOL_TAG) CMiniportTopology(OuterUnknown);
    if (Object == NULL)
    {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Object->AddRef();
    *Unknown = reinterpret_cast<PUNKNOWN>(Object);
    return STATUS_SUCCESS;
}

/* ------------------------------------------------------------------------- */
/* Miniport                                                                  */
/* ------------------------------------------------------------------------- */

CMiniportTopology::CMiniportTopology(_In_opt_ PUNKNOWN OuterUnknown)
    : CUnknown(OuterUnknown)
{
}

CMiniportTopology::~CMiniportTopology()
{
}

STDMETHODIMP
CMiniportTopology::NonDelegatingQueryInterface(_In_ REFIID Interface, _Out_ PVOID *Object)
{
    PAGED_CODE();

    if (Object == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }

    if (IsEqualGUIDAligned(Interface, IID_IUnknown))
    {
        *Object = PVOID(PUNKNOWN(PMINIPORTTOPOLOGY(this)));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniport))
    {
        *Object = PVOID(PMINIPORT(this));
    }
    else if (IsEqualGUIDAligned(Interface, IID_IMiniportTopology))
    {
        *Object = PVOID(PMINIPORTTOPOLOGY(this));
    }
    else
    {
        return STATUS_INVALID_PARAMETER;
    }

    PUNKNOWN(*Object)->AddRef();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportTopology::Init(_In_ PUNKNOWN UnknownAdapter, _In_ PRESOURCELIST ResourceList,
                        _In_ PPORTTOPOLOGY Port)
{
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);
    PAGED_CODE();
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportTopology::GetDescription(_Out_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor)
{
    PAGED_CODE();

    if (OutFilterDescriptor == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    *OutFilterDescriptor = &g_TopoFilterDescriptor;
    return STATUS_SUCCESS;
}

STDMETHODIMP_(NTSTATUS)
CMiniportTopology::DataRangeIntersection(_In_ ULONG PinId,
                                         _In_ PKSDATARANGE ClientDataRange,
                                         _In_ PKSDATARANGE MyDataRange,
                                         _In_ ULONG OutputBufferLength,
                                         _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength)
                                              PVOID ResultantFormat,
                                         _Out_ PULONG ResultantFormatLength)
{
    UNREFERENCED_PARAMETER(PinId);
    UNREFERENCED_PARAMETER(ClientDataRange);
    UNREFERENCED_PARAMETER(MyDataRange);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(ResultantFormat);
    UNREFERENCED_PARAMETER(ResultantFormatLength);
    return STATUS_NOT_IMPLEMENTED;
}

/* ------------------------------------------------------------------------- */
/* Jack properties                                                           */
/* ------------------------------------------------------------------------- */

static NTSTATUS MhVmicTopologyPropertyHandler(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    if (PropertyRequest == NULL || PropertyRequest->MajorTarget == NULL)
    {
        return STATUS_INVALID_PARAMETER;
    }
    return ((CMiniportTopology *)PropertyRequest->MajorTarget)->PropertyHandler(PropertyRequest);
}

static NTSTATUS MhVmicJackBasicSupport(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    if (PropertyRequest->ValueSize == 0)
    {
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_BUFFER_OVERFLOW;
    }
    if (PropertyRequest->ValueSize < sizeof(ULONG))
    {
        return STATUS_BUFFER_TOO_SMALL;
    }
    *(PULONG)PropertyRequest->Value = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT;
    return STATUS_SUCCESS;
}

NTSTATUS CMiniportTopology::PropertyHandler(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    PAGED_CODE();

    if (PropertyRequest->PropertyItem == NULL ||
        !IsEqualGUIDAligned(*PropertyRequest->PropertyItem->Set, KSPROPSETID_Jack))
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }

    if (PropertyRequest->PropertyItem->Id == KSPROPERTY_JACK_DESCRIPTION)
    {
        return HandleJackDescription(PropertyRequest);
    }
    if (PropertyRequest->PropertyItem->Id == KSPROPERTY_JACK_DESCRIPTION2)
    {
        return HandleJackDescription2(PropertyRequest);
    }
    return STATUS_INVALID_DEVICE_REQUEST;
}

NTSTATUS CMiniportTopology::HandleJackDescription(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG Pin = 0;
    ULONG Needed = sizeof(KSMULTIPLE_ITEM) + sizeof(KSJACK_DESCRIPTION);

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT)
    {
        return MhVmicJackBasicSupport(PropertyRequest);
    }
    if (!(PropertyRequest->Verb & KSPROPERTY_TYPE_GET))
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if (PropertyRequest->InstanceSize >= sizeof(ULONG) && PropertyRequest->Instance != NULL)
    {
        Pin = *(PULONG)PropertyRequest->Instance;
    }
    if (Pin != KSPIN_TOPO_MIC)
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }

    if (PropertyRequest->ValueSize == 0)
    {
        PropertyRequest->ValueSize = Needed;
        return STATUS_BUFFER_OVERFLOW;
    }
    if (PropertyRequest->ValueSize < Needed)
    {
        return STATUS_BUFFER_TOO_SMALL;
    }

    {
        PKSMULTIPLE_ITEM MultipleItem = (PKSMULTIPLE_ITEM)PropertyRequest->Value;
        PKSJACK_DESCRIPTION Description = (PKSJACK_DESCRIPTION)(MultipleItem + 1);

        MultipleItem->Size = Needed;
        MultipleItem->Count = 1;

        Description->ChannelMapping = 0;
        Description->Color = 0;
        Description->ConnectionType = eConnTypeUnknown;
        Description->GeoLocation = eGeoLocFront;
        Description->GenLocation = eGenLocPrimaryBox;
        Description->PortConnection = ePortConnIntegratedDevice;
        Description->IsConnected = TRUE;
    }
    return STATUS_SUCCESS;
}

NTSTATUS CMiniportTopology::HandleJackDescription2(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG Pin = 0;
    ULONG Needed = sizeof(KSMULTIPLE_ITEM) + sizeof(KSJACK_DESCRIPTION2);

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT)
    {
        return MhVmicJackBasicSupport(PropertyRequest);
    }
    if (!(PropertyRequest->Verb & KSPROPERTY_TYPE_GET))
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if (PropertyRequest->InstanceSize >= sizeof(ULONG) && PropertyRequest->Instance != NULL)
    {
        Pin = *(PULONG)PropertyRequest->Instance;
    }
    if (Pin != KSPIN_TOPO_MIC)
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }

    if (PropertyRequest->ValueSize == 0)
    {
        PropertyRequest->ValueSize = Needed;
        return STATUS_BUFFER_OVERFLOW;
    }
    if (PropertyRequest->ValueSize < Needed)
    {
        return STATUS_BUFFER_TOO_SMALL;
    }

    {
        PKSMULTIPLE_ITEM MultipleItem = (PKSMULTIPLE_ITEM)PropertyRequest->Value;
        PKSJACK_DESCRIPTION2 Description = (PKSJACK_DESCRIPTION2)(MultipleItem + 1);

        MultipleItem->Size = Needed;
        MultipleItem->Count = 1;
        RtlZeroMemory(Description, sizeof(*Description));
        Description->DeviceStateInfo = 0;
        Description->JackCapabilities = 0;
    }
    return STATUS_SUCCESS;
}
