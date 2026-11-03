#include "host_internal.h"

#include <objbase.h>
#include <windows.h>

using namespace michost;

namespace {

struct ComInitializer {
    HRESULT result;
    explicit ComInitializer(DWORD model) : result(CoInitializeEx(nullptr, model)) {}
};

}

namespace michost {

void prepareCom()
{
    thread_local ComInitializer initializer(COINIT_APARTMENTTHREADED);
    (void)initializer;
}

void prepareComGui()
{
    thread_local ComInitializer initializer(COINIT_APARTMENTTHREADED);
    (void)initializer;
}

}

extern "C" {

uint32_t mh_abi_version(void)
{
    return MH_ABI_VERSION;
}

uint32_t mh_layout_size(int32_t kind)
{
    switch (kind) {
    case MH_LAYOUT_DEVICE_INFO:
        return static_cast<uint32_t>(sizeof(MHDeviceInfo));
    case MH_LAYOUT_STREAM_CONFIG:
        return static_cast<uint32_t>(sizeof(MHStreamConfig));
    case MH_LAYOUT_STREAM_CALLBACKS:
        return static_cast<uint32_t>(sizeof(MHStreamCallbacks));
    case MH_LAYOUT_LATENCY:
        return static_cast<uint32_t>(sizeof(MHLatency));
    case MH_LAYOUT_PLUGIN_DESCRIPTOR:
        return static_cast<uint32_t>(sizeof(MHPluginDescriptor));
    case MH_LAYOUT_PARAM_INFO:
        return static_cast<uint32_t>(sizeof(MHParamInfo));
    case MH_LAYOUT_CALLBACKS:
        return static_cast<uint32_t>(sizeof(MHCallbacks));
    default:
        return 0;
    }
}

int32_t mh_init(const MHCallbacks* callbacks, void** out_manager)
{
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);

    if (out_manager == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_manager = nullptr;

    try {
        auto host = std::make_unique<Host>();
        host->formats.addDefaultFormats();

        host->gui = std::make_unique<GuiThread>();
        if (!host->gui->startThread()) {
            host->gui.reset();
            return MH_ERR;
        }

        if (callbacks != nullptr)
            publishCallbacks(*host, *callbacks);

        *out_manager = host.release();
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_set_callbacks(void* manager, const MHCallbacks* callbacks)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || callbacks == nullptr)
        return MH_INVALID_ARGUMENT;
    if (callbacks->abi_version != 0 && callbacks->abi_version != MH_ABI_VERSION)
        return MH_ABI_MISMATCH;

    try {
        publishCallbacks(*host, *callbacks);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_set_device_change_callback(void* manager,
                                      MHDeviceChangedCallback callback,
                                      void* ctx)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr)
        return MH_INVALID_ARGUMENT;

    try {
        host->setDeviceChangeCallback(callback, ctx);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

void mh_shutdown(void* manager)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr)
        return;

    try {
        delete host;
    } catch (...) {
    }
}

}
