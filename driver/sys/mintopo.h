#ifndef MH_VMIC_MINTOPO_H
#define MH_VMIC_MINTOPO_H

#include "driver.h"

class CMiniportTopology : public IMiniportTopology, public CUnknown
{
public:
    CMiniportTopology(_In_opt_ PUNKNOWN OuterUnknown);
    ~CMiniportTopology();

    DECLARE_STD_UNKNOWN();
    IMP_IMiniportTopology;

    NTSTATUS PropertyHandler(_In_ PPCPROPERTY_REQUEST PropertyRequest);

private:
    NTSTATUS HandleJackDescription(_In_ PPCPROPERTY_REQUEST PropertyRequest);
    NTSTATUS HandleJackDescription2(_In_ PPCPROPERTY_REQUEST PropertyRequest);
};

#endif /* MH_VMIC_MINTOPO_H */
