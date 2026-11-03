#include "host_internal.h"

#include <cmath>

using namespace michost;

namespace {

std::mutex streamRegistryMutex;
std::map<void*, DeviceStream*> streamRegistry;

int32_t apiForType(const juce::String& typeName)
{
    if (typeName.containsIgnoreCase("ASIO"))
        return MH_API_ASIO;
    if (typeName.containsIgnoreCase("Low Latency"))
        return MH_API_WASAPI_LOW_LATENCY;
    if (typeName.containsIgnoreCase("Exclusive"))
        return MH_API_WASAPI_EXCLUSIVE;
    if (typeName.containsIgnoreCase("Windows Audio"))
        return MH_API_WASAPI_SHARED;
    return -1;
}

double preferredSampleRate(juce::AudioIODevice& device)
{
    const auto current = device.getCurrentSampleRate();
    if (current > 0.0)
        return current;

    const auto rates = device.getAvailableSampleRates();
    if (rates.contains(48000.0))
        return 48000.0;
    if (rates.contains(44100.0))
        return 44100.0;
    if (!rates.isEmpty())
        return rates.getFirst();
    return 48000.0;
}

std::string capabilityKey(int32_t api, const juce::String& name, bool wantInput)
{
    return std::to_string(api) + "|" + name.toStdString() + (wantInput ? "|i" : "|o");
}

DeviceCapabilityInfo queryCapabilities(Host& host,
                                       juce::AudioIODeviceType& type,
                                       const juce::String& name,
                                       bool wantInput)
{
    const auto api = apiForType(type.getTypeName());
    const auto key = capabilityKey(api, name, wantInput);
    {
        const std::lock_guard<std::mutex> lock(host.capabilitiesMutex);
        const auto found = host.capabilities.find(key);
        if (found != host.capabilities.end())
            return found->second;
    }

    if (api == MH_API_ASIO && host.asioReserved()) {
        DeviceCapabilityInfo caps;
        caps.inputs = 2;
        caps.outputs = 2;
        return caps;
    }

    std::unique_ptr<juce::AudioIODevice> device;
    if (wantInput)
        device.reset(type.createDevice({}, name));
    else
        device.reset(type.createDevice(name, {}));
    if (device == nullptr)
        device.reset(type.createDevice(name, name));

    DeviceCapabilityInfo caps;
    if (device != nullptr) {
        caps.inputs = device->getInputChannelNames().size();
        caps.outputs = device->getOutputChannelNames().size();
        caps.sampleRate = preferredSampleRate(*device);
        caps.hasControlPanel = device->hasControlPanel();

        const auto buffers = device->getAvailableBufferSizes();
        for (const auto size : buffers)
            if (size > 0)
                caps.bufferSizes.push_back(size);
        const auto defaultBuffer = device->getDefaultBufferSize();
        if (defaultBuffer > 0
            && std::find(caps.bufferSizes.begin(), caps.bufferSizes.end(), defaultBuffer) == caps.bufferSizes.end())
            caps.bufferSizes.push_back(defaultBuffer);
    }
    if (wantInput && caps.inputs == 0)
        caps.inputs = 2;
    if (!wantInput && caps.outputs == 0)
        caps.outputs = 2;
    if (caps.sampleRate <= 0.0)
        caps.sampleRate = 48000.0;

    {
        const std::lock_guard<std::mutex> lock(host.capabilitiesMutex);
        host.capabilities.emplace(key, caps);
    }
    return caps;
}

int32_t enumerateDevices(Host& host, int32_t kind, MHDeviceInfo* out, int32_t max, int32_t* count)
{
    prepareCom();
    if (count == nullptr || max < 0 || (max > 0 && out == nullptr))
        return MH_INVALID_ARGUMENT;
    *count = 0;
    if (kind != MH_DEVICE_KIND_INPUT && kind != MH_DEVICE_KIND_OUTPUT)
        return MH_INVALID_ARGUMENT;

    const auto wantInput = kind == MH_DEVICE_KIND_INPUT;
    int32_t written = 0;
    size_t total = 0;
    {
        const std::lock_guard<std::mutex> lock(host.capabilitiesMutex);
        host.capabilities.clear();
    }

    for (auto* type : host.devices.getAvailableDeviceTypes()) {
        if (type == nullptr)
            continue;
        type->scanForDevices();
        const auto api = apiForType(type->getTypeName());
        if (api < 0)
            continue;

        const auto names = type->getDeviceNames(wantInput);
        const auto defaultIndex = type->getDefaultDeviceIndex(wantInput);

        for (int index = 0; index < names.size(); ++index) {
            if (written < max) {
                const auto& name = names[index];
                const auto caps = queryCapabilities(host, *type, name, wantInput);

                auto& info = out[written];
                std::memset(&info, 0, sizeof(info));
                writeString(info.name, name);
                writeString(info.id, juce::String(api) + "|" + name);
                info.inputs = static_cast<uint32_t>(juce::jmax(0, caps.inputs));
                info.outputs = static_cast<uint32_t>(juce::jmax(0, caps.outputs));
                info.default_sample_rate = caps.sampleRate;
                info.api = api;
                info.is_default = (index == defaultIndex || (defaultIndex < 0 && index == 0)) ? 1 : 0;
                info.has_control_panel = caps.hasControlPanel ? 1 : 0;
                info.buffer_size_count = static_cast<uint32_t>(
                    std::min<size_t>(caps.bufferSizes.size(), MH_MAX_BUFFER_SIZES));
                for (uint32_t slot = 0; slot < info.buffer_size_count; ++slot)
                    info.buffer_sizes[slot] = static_cast<uint32_t>(caps.bufferSizes[slot]);
                ++written;
            }
            ++total;
        }
    }

    *count = static_cast<int32_t>(std::min<size_t>(total, 0x7fffffff));
    return MH_OK;
}

}

namespace michost {

DeviceStream* findStream(void* handle)
{
    const std::lock_guard<std::mutex> lock(streamRegistryMutex);
    const auto found = streamRegistry.find(handle);
    return found != streamRegistry.end() ? found->second : nullptr;
}

void registerStream(DeviceStream* stream)
{
    if (stream == nullptr)
        return;
    const std::lock_guard<std::mutex> lock(streamRegistryMutex);
    streamRegistry[stream] = stream;
}

void unregisterStream(DeviceStream* stream)
{
    const std::lock_guard<std::mutex> lock(streamRegistryMutex);
    streamRegistry.erase(stream);
}

Host::Host() = default;

Host::~Host()
{
    stopDeviceChangeNotifications();

    try {
        closeStreams();
    } catch (...) {
    }

    if (gui != nullptr) {
        try {
            gui->callSync([this] { destroyPlugins(); });
        } catch (...) {
            try {
                destroyPlugins();
            } catch (...) {
            }
        }
        gui->stop();
    } else {
        try {
            destroyPlugins();
        } catch (...) {
        }
    }

    try {
        devices.closeAudioDevice();
    } catch (...) {
    }
}

void Host::setDeviceChangeCallback(MHDeviceChangedCallback callback, void* context)
{
    if (callback == nullptr) {
        deviceChangeTarget.store(nullptr, std::memory_order_release);
        return;
    }

    auto target = std::make_unique<DeviceChangeTarget>();
    target->callback = callback;
    target->context = context;

    const std::lock_guard<std::mutex> lock(deviceChangeMutex);
    deviceChangeTargets.push_back(std::move(target));
    deviceChangeTarget.store(deviceChangeTargets.back().get(), std::memory_order_release);
}

void Host::notifyDeviceChanged() noexcept
{
    const auto* target = deviceChangeTarget.load(std::memory_order_acquire);
    if (target == nullptr || target->callback == nullptr)
        return;
    target->callback(target->context);
}

void Host::ensureDeviceChangeNotifications()
{
    std::call_once(deviceChangeOnce, [this] { startDeviceChangeNotifications(); });
}

void Host::startDeviceChangeNotifications()
{
    if (deviceChangeListener != nullptr)
        return;

    auto listener = std::make_unique<DeviceTypeListener>(*this);
    const auto& types = devices.getAvailableDeviceTypes();
    deviceChangeListener = std::move(listener);
    for (auto* type : types)
        if (type != nullptr)
            type->addListener(deviceChangeListener.get());
}

void Host::stopDeviceChangeNotifications()
{
    deviceChangeTarget.store(nullptr, std::memory_order_release);

    if (deviceChangeListener == nullptr)
        return;

    for (auto* type : devices.getAvailableDeviceTypes())
        if (type != nullptr)
            type->removeListener(deviceChangeListener.get());
    deviceChangeListener.reset();
}

void Host::closeStreams()
{
    std::vector<DeviceStream*> closing;
    {
        const std::lock_guard<std::mutex> lock(streamMutex);
        closing.swap(streams);
    }

    for (auto* stream : closing) {
        if (stream == nullptr)
            continue;
        try {
            stream->stop();
            stream->close();
        } catch (...) {
        }
        unregisterStream(stream);
        delete stream;
    }
}

bool Host::asioReserved()
{
    const std::lock_guard<std::mutex> lock(asioMutex);
    return !asioReservations.empty();
}

int32_t Host::reserveAsio(const juce::String& name)
{
    const auto key = name.toStdString();
    const std::lock_guard<std::mutex> lock(asioMutex);

    ++asioReservations[key];
    return MH_OK;
}

void Host::releaseAsio(const juce::String& name)
{
    const auto key = name.toStdString();
    const std::lock_guard<std::mutex> lock(asioMutex);
    const auto found = asioReservations.find(key);
    if (found == asioReservations.end())
        return;
    if (--found->second <= 0)
        asioReservations.erase(found);
}

DeviceStream::DeviceStream(Host& hostToUse, const MHStreamConfig& configToUse, const MHStreamCallbacks& callbacksToUse)
    : host(hostToUse), config(configToUse), callbacks(callbacksToUse)
{
}

DeviceStream::~DeviceStream()
{
    close();
}

void DeviceStream::report(int32_t status, const juce::String& message)
{
    if (callbacks.on_error != nullptr)
        callbacks.on_error(callbacks.ctx, status, message.toRawUTF8());
    reportError(host, status, message);
}

int32_t DeviceStream::open()
{
    prepareCom();

    const auto name = deviceName();
    if (name.isEmpty() || config.channels == 0) {
        report(MH_INVALID_ARGUMENT, "the stream configuration needs a device name and at least one channel");
        return MH_INVALID_ARGUMENT;
    }
    if (config.api < MH_API_WASAPI_SHARED || config.api > MH_API_ASIO) {
        report(MH_INVALID_ARGUMENT, "the stream configuration has an unknown API");
        return MH_INVALID_ARGUMENT;
    }

    juce::AudioIODeviceType* chosen = nullptr;
    for (auto* type : host.devices.getAvailableDeviceTypes()) {
        if (type == nullptr)
            continue;
        type->scanForDevices();
        if (apiForType(type->getTypeName()) == config.api) {
            chosen = type;
            break;
        }
    }
    if (chosen == nullptr) {
        report(MH_NOT_FOUND, "the requested audio API is not available");
        return MH_NOT_FOUND;
    }

    if (config.api == MH_API_ASIO) {
        const auto status = host.reserveAsio(name);
        if (status != MH_OK) {
            report(status, "could not reserve the ASIO driver");
            return status;
        }
        asioReserved = true;
    }

    const bool duplex = wantsInput() && config.output_channels > 0;
    deviceType = chosen;
    if (duplex)
        device.reset(deviceType->createDevice(name, name));
    else if (wantsInput())
        device.reset(deviceType->createDevice({}, name));
    else
        device.reset(deviceType->createDevice(name, {}));
    if (device == nullptr)
        device.reset(deviceType->createDevice(name, name));
    if (device == nullptr) {
        report(MH_NOT_FOUND, "could not create the device '" + name + "'");
        close();
        return MH_NOT_FOUND;
    }

    const auto buildMask = [&](uint32_t count,
                               uint64_t bits,
                               int available,
                               const char* label,
                               juce::BigInteger& mask) -> bool {
        if (available > 0 && static_cast<int>(count) > available) {
            report(MH_ERR, "'" + name + "' offers " + juce::String(available) + " "
                               + label + " channels, but " + juce::String(count)
                               + " were requested");
            return false;
        }
        if (bits != 0) {
            for (int bit = 0; bit < 64; ++bit)
                if ((bits >> bit) & 1ull)
                    mask.setBit(bit);
            if (mask.countNumberOfSetBits() != static_cast<int>(count)) {
                report(MH_ERR, juce::String("the ") + label + " channel mask selects "
                                   + juce::String(mask.countNumberOfSetBits())
                                   + " channels, but " + juce::String(count)
                                   + " were requested");
                return false;
            }
            if (available > 0 && mask.getHighestBit() >= available) {
                report(MH_ERR, "'" + name + "' offers only " + juce::String(available)
                                   + " " + label + " channels for the selected mask");
                return false;
            }
        } else {
            mask.setRange(0, static_cast<int>(count), true);
        }
        return true;
    };

    const int inputChannels = wantsInput() ? static_cast<int>(config.channels) : 0;
    const int outputChannels = duplex ? static_cast<int>(config.output_channels)
                                      : (!wantsInput() ? static_cast<int>(config.channels) : 0);
    const int availableInput = static_cast<int>(device->getInputChannelNames().size());
    const int availableOutput = static_cast<int>(device->getOutputChannelNames().size());

    juce::BigInteger inputMask;
    juce::BigInteger outputMask;
    if (inputChannels > 0
        && !buildMask(config.channels, config.channel_mask, availableInput, "input", inputMask)) {
        close();
        return MH_ERR;
    }
    if (outputChannels > 0
        && !buildMask(static_cast<uint32_t>(outputChannels),
                      duplex ? config.output_channel_mask : config.channel_mask,
                      availableOutput,
                      "output",
                      outputMask)) {
        close();
        return MH_ERR;
    }

    const auto requestedRate = config.sample_rate > 0.0 ? config.sample_rate : 0.0;
    const auto requestedBlock = config.block_size > 0 ? static_cast<int>(config.block_size) : 0;

    auto error = device->open(inputMask, outputMask, requestedRate, requestedBlock);

    if (error.isNotEmpty()) {
        report(MH_ERR, "opening '" + name + "': " + error);
        close();
        return MH_ERR;
    }

    return MH_OK;
}

int32_t DeviceStream::start()
{
    if (device == nullptr) {
        report(MH_ERR, "the stream is not open");
        return MH_ERR;
    }
    if (!running.exchange(true))
        device->start(this);
    return MH_OK;
}

int32_t DeviceStream::stop()
{
    if (running.exchange(false) && device != nullptr)
        device->stop();
    return MH_OK;
}

int32_t DeviceStream::close()
{
    stop();
    device.reset();
    deviceType = nullptr;
    if (asioReserved) {
        host.releaseAsio(deviceName());
        asioReserved = false;
    }
    return MH_OK;
}

int32_t DeviceStream::latency(MHLatency& out) const
{
    if (device == nullptr)
        return MH_ERR;

    out.input = static_cast<uint32_t>(juce::jmax(0, device->getInputLatencyInSamples()));
    out.output = static_cast<uint32_t>(juce::jmax(0, device->getOutputLatencyInSamples()));
    out.buffer = static_cast<uint32_t>(juce::jmax(0, device->getCurrentBufferSizeSamples()));
    out.sample_rate = static_cast<uint32_t>(juce::jmax(0.0, device->getCurrentSampleRate()) + 0.5);
    return MH_OK;
}

int32_t DeviceStream::controlPanel()
{
    if (device == nullptr)
        return MH_ERR;
    if (!device->hasControlPanel()) {
        report(MH_NOT_SUPPORTED, "This device does not provide a control panel.");
        return MH_NOT_SUPPORTED;
    }

    if (host.gui != nullptr)
        return host.gui->callSync([this]() -> int32_t {
            if (device == nullptr)
                return MH_ERR;
            device->showControlPanel();
            return MH_OK;
        });

    device->showControlPanel();
    return MH_OK;
}

void DeviceStream::audioDeviceIOCallbackWithContext(const float* const* inputChannelData,
                                                    int numInputChannels,
                                                    float* const* outputChannelData,
                                                    int numOutputChannels,
                                                    int numSamples,
                                                    const juce::AudioIODeviceCallbackContext&)
{
    if (!running.load(std::memory_order_acquire)) {
        for (int channel = 0; channel < numOutputChannels; ++channel)
            if (outputChannelData[channel] != nullptr)
                juce::FloatVectorOperations::clear(outputChannelData[channel], numSamples);
        return;
    }

    const auto position = samplePosition.fetch_add(numSamples, std::memory_order_relaxed);

    if (wantsInput() && callbacks.on_input != nullptr)
        callbacks.on_input(callbacks.ctx, inputChannelData, numInputChannels, numSamples, position);

    if (wantsInput() && config.output_channels == 0)
        return;

    for (int channel = 0; channel < numOutputChannels; ++channel)
        if (outputChannelData[channel] != nullptr)
            juce::FloatVectorOperations::clear(outputChannelData[channel], numSamples);

    if (callbacks.on_output != nullptr)
        callbacks.on_output(callbacks.ctx, outputChannelData, numOutputChannels, numSamples, position);
}

void DeviceStream::audioDeviceAboutToStart(juce::AudioIODevice*)
{
    samplePosition.store(0, std::memory_order_relaxed);
}

void DeviceStream::audioDeviceStopped()
{
}

void DeviceStream::audioDeviceError(const juce::String& message)
{
    if (callbacks.on_error != nullptr)
        callbacks.on_error(callbacks.ctx, MH_ERR, message.toRawUTF8());
    reportError(host, MH_ERR, message);
}

}

extern "C" {

int32_t mh_device_enumerate(void* manager, int32_t kind, MHDeviceInfo* out, int32_t max, int32_t* count)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr)
        return MH_INVALID_ARGUMENT;
    try {
        host->ensureDeviceChangeNotifications();
        return enumerateDevices(*host, kind, out, max, count);
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_open(void* manager, const MHStreamConfig* config, const MHStreamCallbacks* callbacks, void** out_stream)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || config == nullptr || callbacks == nullptr || out_stream == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_stream = nullptr;
    if (config->direction != MH_STREAM_INPUT && config->direction != MH_STREAM_OUTPUT)
        return MH_INVALID_ARGUMENT;

    try {
        auto stream = std::make_unique<DeviceStream>(*host, *config, *callbacks);
        const auto status = stream->open();
        if (status != MH_OK)
            return status;

        registerStream(stream.get());
        try {
            const std::lock_guard<std::mutex> lock(host->streamMutex);
            host->streams.push_back(stream.get());
        } catch (...) {
            unregisterStream(stream.get());
            throw;
        }
        *out_stream = stream.release();
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_start(void* stream_handle)
{
    auto* stream = findStream(stream_handle);
    if (stream == nullptr)
        return MH_ERR;
    try {
        return stream->start();
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_stop(void* stream_handle)
{
    auto* stream = findStream(stream_handle);
    if (stream == nullptr)
        return MH_OK;
    try {
        return stream->stop();
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_close(void* stream_handle)
{
    auto* stream = findStream(stream_handle);
    if (stream == nullptr)
        return MH_OK;

    try {
        auto& host = stream->owner();
        stream->stop();
        stream->close();
        {
            const std::lock_guard<std::mutex> lock(host.streamMutex);
            auto& streams = host.streams;
            streams.erase(std::remove(streams.begin(), streams.end(), stream), streams.end());
        }
        unregisterStream(stream);
        delete stream;
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_latency(void* stream_handle, MHLatency* out)
{
    auto* stream = findStream(stream_handle);
    if (stream == nullptr || out == nullptr)
        return MH_INVALID_ARGUMENT;
    try {
        return stream->latency(*out);
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_stream_control_panel(void* stream_handle)
{
    auto* stream = findStream(stream_handle);
    if (stream == nullptr)
        return MH_INVALID_ARGUMENT;
    try {
        return stream->controlPanel();
    } catch (...) {
        return MH_ERR;
    }
}

}
