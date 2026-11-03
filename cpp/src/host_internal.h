#pragma once

#include "mic_host.h"

#include <juce_audio_devices/juce_audio_devices.h>
#include <juce_audio_processors/juce_audio_processors.h>
#include <juce_gui_basics/juce_gui_basics.h>

#include <algorithm>
#include <atomic>
#include <cstddef>
#include <cstring>
#include <deque>
#include <functional>
#include <future>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <type_traits>
#include <vector>

namespace michost {

class GuiThread final : public juce::Thread {
public:
    GuiThread() : juce::Thread("michost-gui") {}

    void run() override;
    void stop();

    void post(std::function<void()> task);
    bool isRunning() const noexcept { return running.load(std::memory_order_acquire); }

    template <typename Fn>
    auto callSync(Fn&& fn) -> decltype(fn())
    {
        using Result = decltype(fn());
        if (!isRunning() || juce::Thread::getCurrentThreadId() == getThreadId())
            return runInline<Result>(std::forward<Fn>(fn));

        auto promise = std::make_shared<std::promise<Result>>();
        auto future = promise->get_future();
        post([fn = std::forward<Fn>(fn), promise]() mutable {
            try {
                if constexpr (std::is_void_v<Result>) {
                    fn();
                    promise->set_value();
                } else {
                    promise->set_value(fn());
                }
            } catch (...) {
                promise->set_exception(std::current_exception());
            }
        });
        return future.get();
    }

private:
    template <typename Result, typename Fn>
    static Result runInline(Fn&& fn)
    {
        if constexpr (std::is_void_v<Result>) {
            fn();
        } else {
            return fn();
        }
    }

    void drain();

    std::mutex mutex;
    std::deque<std::function<void()>> tasks;
    std::atomic<bool> running{false};
};

struct Host;

struct PluginChangeListener final : juce::AudioProcessorListener {
    PluginChangeListener(std::atomic<bool>& edited, const std::atomic<bool>& editorOpen)
        : edited(edited), editorOpen(editorOpen) {}

    void audioProcessorParameterChanged(juce::AudioProcessor*, int, float) override
    {
        if (!editorOpen.load(std::memory_order_relaxed))
            return;
        edited.store(true, std::memory_order_release);
    }

    void audioProcessorChanged(juce::AudioProcessor*, const ChangeDetails& details) override
    {
        if (!editorOpen.load(std::memory_order_relaxed))
            return;
        if (!details.parameterInfoChanged && !details.programChanged
            && !details.nonParameterStateChanged)
            return;
        edited.store(true, std::memory_order_release);
    }

    std::atomic<bool>& edited;
    const std::atomic<bool>& editorOpen;
};

struct PluginInstance {
    ~PluginInstance()
    {
        if (processor != nullptr && editListener != nullptr)
            processor->removeListener(editListener.get());
    }

    std::unique_ptr<juce::AudioProcessor> processor;
    std::unique_ptr<juce::AudioProcessorEditor> editor;
    std::unique_ptr<juce::DocumentWindow> editorWindow;
    juce::String path;
    juce::String classId;
    double sampleRate = 48000.0;
    int blockSize = 256;
    juce::AudioBuffer<float> processBuffer;
    int processFrames = 0;
    std::atomic<int> totalInputChannels{0};
    std::atomic<int> totalOutputChannels{0};
    std::atomic<int> mainInputChannels{0};
    std::atomic<int> mainOutputChannels{0};
    std::atomic<bool> prepared{false};
    std::atomic<bool> active{false};
    std::atomic<bool> resourcesReleased{false};
    std::atomic<bool> edited{false};
    std::atomic<bool> editorOpen{false};
    std::unique_ptr<PluginChangeListener> editListener;
    Host* host = nullptr;
};

struct DeviceCapabilityInfo {
    int inputs = 0;
    int outputs = 0;
    double sampleRate = 0.0;
    bool hasControlPanel = false;
    std::vector<int> bufferSizes;
};

class DeviceStream final : public juce::AudioIODeviceCallback {
public:
    DeviceStream(Host& hostToUse, const MHStreamConfig& configToUse, const MHStreamCallbacks& callbacksToUse);
    ~DeviceStream() override;

    int32_t open();
    int32_t start();
    int32_t stop();
    int32_t close();
    int32_t latency(MHLatency& out) const;
    int32_t controlPanel();

    Host& owner() const noexcept { return host; }
    bool wantsInput() const noexcept { return config.direction == MH_STREAM_INPUT; }
    int32_t api() const noexcept { return config.api; }
    juce::String deviceName() const { return juce::String::fromUTF8(config.device); }

    void audioDeviceIOCallbackWithContext(const float* const* inputChannelData,
                                          int numInputChannels,
                                          float* const* outputChannelData,
                                          int numOutputChannels,
                                          int numSamples,
                                          const juce::AudioIODeviceCallbackContext& context) override;
    void audioDeviceAboutToStart(juce::AudioIODevice* device) override;
    void audioDeviceStopped() override;
    void audioDeviceError(const juce::String& message) override;

private:
    void report(int32_t status, const juce::String& message);

    Host& host;
    MHStreamConfig config{};
    MHStreamCallbacks callbacks{};
    juce::AudioIODeviceType* deviceType = nullptr;
    std::unique_ptr<juce::AudioIODevice> device;
    std::atomic<bool> running{false};
    std::atomic<int64_t> samplePosition{0};
    bool asioReserved = false;
};

struct DeviceChangeTarget {
    MHDeviceChangedCallback callback = nullptr;
    void* context = nullptr;
};

struct Host {
    juce::AudioPluginFormatManager formats;
    juce::AudioDeviceManager devices;

    std::atomic<const MHCallbacks*> callbacks{nullptr};
    std::vector<std::unique_ptr<MHCallbacks>> callbackHistory;
    std::mutex callbackMutex;

    struct DeviceTypeListener final : juce::AudioIODeviceType::Listener {
        explicit DeviceTypeListener(Host& hostToUse) noexcept : owner(hostToUse) {}
        void audioDeviceListChanged() override { owner.notifyDeviceChanged(); }
        Host& owner;
    };

    std::atomic<const DeviceChangeTarget*> deviceChangeTarget{nullptr};
    std::vector<std::unique_ptr<DeviceChangeTarget>> deviceChangeTargets;
    std::mutex deviceChangeMutex;
    std::unique_ptr<DeviceTypeListener> deviceChangeListener;
    std::once_flag deviceChangeOnce;

    std::unique_ptr<GuiThread> gui;

    std::mutex streamMutex;
    std::vector<DeviceStream*> streams;

    std::mutex pluginMutex;
    std::vector<PluginInstance*> plugins;

    std::mutex deviceMutex;

    std::mutex capabilitiesMutex;
    std::map<std::string, DeviceCapabilityInfo> capabilities;

    std::mutex asioMutex;
    std::map<std::string, int> asioReservations;

    Host();
    ~Host();

    void setDeviceChangeCallback(MHDeviceChangedCallback callback, void* context);
    void notifyDeviceChanged() noexcept;
    void ensureDeviceChangeNotifications();
    void startDeviceChangeNotifications();
    void stopDeviceChangeNotifications();

    void closeStreams();
    void destroyPlugins();
    bool asioReserved();
    int32_t reserveAsio(const juce::String& name);
    void releaseAsio(const juce::String& name);
};

DeviceStream* findStream(void* handle);
void registerStream(DeviceStream* stream);
void unregisterStream(DeviceStream* stream);

void closePluginEditor(PluginInstance& instance);

void prepareCom();
void prepareComGui();

inline juce::String toJuce(const char* text)
{
    return text != nullptr ? juce::String::fromUTF8(text) : juce::String();
}

template <size_t N>
inline void writeString(char (&destination)[N], const juce::String& text)
{
    std::memset(destination, 0, N);
    const auto* utf8 = text.toRawUTF8();
    const auto length = std::min<size_t>(std::strlen(utf8), N - 1);
    std::memcpy(destination, utf8, length);
}

inline void publishCallbacks(Host& host, const MHCallbacks& callbacks)
{
    auto copy = std::make_unique<MHCallbacks>(callbacks);
    copy->abi_version = MH_ABI_VERSION;
    copy->size = static_cast<uint32_t>(sizeof(MHCallbacks));

    std::lock_guard<std::mutex> lock(host.callbackMutex);
    host.callbackHistory.push_back(std::move(copy));
    host.callbacks.store(host.callbackHistory.back().get(), std::memory_order_release);
}

inline void reportError(Host& host, int32_t status, const juce::String& message)
{
    const auto* callbacks = host.callbacks.load(std::memory_order_acquire);
    if (callbacks != nullptr && callbacks->on_error != nullptr)
        callbacks->on_error(callbacks->ui_ctx, status, message.toRawUTF8());
}

inline void reportLatency(Host& host, PluginInstance& instance)
{
    const auto* callbacks = host.callbacks.load(std::memory_order_acquire);
    if (callbacks != nullptr && callbacks->on_latency_changed != nullptr)
        callbacks->on_latency_changed(callbacks->ui_ctx,
                                      &instance,
                                      juce::jmax(0, instance.processor->getLatencySamples()));
}

}

static_assert(sizeof(void*) != 8 || sizeof(MHCallbacks) == 32, "MHCallbacks layout changed");
static_assert(sizeof(MHDeviceInfo) == 480, "MHDeviceInfo layout changed");
static_assert(sizeof(MHStreamConfig) == 304, "MHStreamConfig layout changed");
static_assert(offsetof(MHStreamConfig, channel_mask) == 280, "MHStreamConfig channel mask offset changed");
static_assert(offsetof(MHStreamConfig, output_channels) == 288, "MHStreamConfig output channels offset changed");
static_assert(offsetof(MHStreamConfig, output_channel_mask) == 296, "MHStreamConfig output mask offset changed");
static_assert(sizeof(void*) != 8 || sizeof(MHStreamCallbacks) == 40, "MHStreamCallbacks layout changed");
static_assert(sizeof(MHLatency) == 16, "MHLatency layout changed");
static_assert(sizeof(MHPluginDescriptor) == 1168, "MHPluginDescriptor layout changed");
static_assert(sizeof(MHParamInfo) == 184, "MHParamInfo layout changed");
static_assert(offsetof(MHDeviceInfo, buffer_sizes) == 408, "MHDeviceInfo buffer_sizes offset changed");
static_assert(offsetof(MHPluginDescriptor, main_inputs) == 1024, "MHPluginDescriptor counts offset changed");
static_assert(offsetof(MHPluginDescriptor, category) == 1040, "MHPluginDescriptor category offset changed");
static_assert(offsetof(MHParamInfo, default_value) == 168, "MHParamInfo default_value offset changed");
