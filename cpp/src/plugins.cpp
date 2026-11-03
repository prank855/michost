#include "host_internal.h"

#if defined(_MSC_VER)
#include <excpt.h>
#endif

#include <array>
#include <cstring>
#include <pluginterfaces/base/funknown.h>
#include <pluginterfaces/base/ipluginbase.h>
#include <pluginterfaces/vst/ivstaudioprocessor.h>

using namespace michost;

namespace {

// Mirrors michost_graph::MAX_FRAMES, the largest block the chain engine can send.
constexpr int maxProcessFrames = 4096;

juce::AudioPluginFormat* findVst3(Host& host)
{
    for (auto* format : host.formats.getFormats())
        if (format != nullptr && format->getName().equalsIgnoreCase("VST3"))
            return format;
    return nullptr;
}


juce::File moduleBinary(const juce::String& path)
{
    juce::File file(path);
    if (!file.isDirectory())
        return file;

    const auto inner = file.getChildFile("Contents")
                           .getChildFile("x86_64-win")
                           .getChildFile(file.getFileName());
    if (inner.existsAsFile())
        return inner;

    auto found = file.findChildFiles(juce::File::findFiles, true, "*.vst3");
    if (!found.isEmpty())
        return found[0];

    found = file.findChildFiles(juce::File::findFiles, true, "*.dll");
    if (!found.isEmpty())
        return found[0];

    return file;
}

int hashNormalised(const Steinberg::TUID& cid)
{
    const Steinberg::FUID fuid(cid);
    const std::array<uint32_t, 4> words {
        fuid.getLong1(), fuid.getLong2(), fuid.getLong3(), fuid.getLong4()
    };
    uint32_t value = 0;
    for (const auto word : words)
        value = (value * 31) + word;
    return static_cast<int>(value);
}

int hashRaw(const Steinberg::TUID& cid)
{
    uint32_t value = 0;
    for (size_t index = 0; index < sizeof(Steinberg::TUID); ++index)
        value = (value * 31) + static_cast<uint32_t>(cid[index]);
    return static_cast<int>(value);
}

std::vector<juce::PluginDescription> enumerateModule(const juce::String& path)
{
    std::vector<juce::PluginDescription> result;
    const auto binary = moduleBinary(path);
    if (!binary.existsAsFile())
        return result;

    juce::DynamicLibrary library;
    if (!library.open(binary.getFullPathName()))
        return result;

    using EntryProc = bool (PLUGIN_API*)();
    auto* init = reinterpret_cast<EntryProc>(library.getFunction("InitDll"));
    if (init != nullptr && !init()) {
        library.close();
        return result;
    }

    using FactoryProc = Steinberg::IPluginFactory* (PLUGIN_API*)();
    auto* proc = reinterpret_cast<FactoryProc>(library.getFunction("GetPluginFactory"));
    if (proc == nullptr) {
        if (auto* exit = reinterpret_cast<EntryProc>(library.getFunction("ExitDll")))
            exit();
        library.close();
        return result;
    }

    Steinberg::IPluginFactory* factory = proc();
    if (factory == nullptr) {
        if (auto* exit = reinterpret_cast<EntryProc>(library.getFunction("ExitDll")))
            exit();
        library.close();
        return result;
    }

    Steinberg::PFactoryInfo factoryInfo {};
    factory->getFactoryInfo(&factoryInfo);
    const juce::String factoryVendor(factoryInfo.vendor);

    Steinberg::IPluginFactory2* factory2 = nullptr;
    const Steinberg::FUID factory2Iid(0x0007B650, 0xF24B4C0B, 0xA464EDB9, 0xF00B2ABB);
    if (factory->queryInterface(factory2Iid, reinterpret_cast<void**>(&factory2)) != Steinberg::kResultOk)
        factory2 = nullptr;

    const auto count = factory->countClasses();
    for (Steinberg::int32 index = 0; index < count; ++index) {
        Steinberg::PClassInfo info {};
        if (factory->getClassInfo(index, &info) != Steinberg::kResultOk)
            continue;
        if (std::strcmp(info.category, kVstAudioEffectClass) != 0)
            continue;

        juce::String name(info.name);
        name = name.trim();
        if (name.isEmpty())
            continue;

        juce::String vendor = factoryVendor.trim();
        juce::String version;
        juce::String category(info.category);
        if (factory2 != nullptr) {
            Steinberg::PClassInfo2 info2 {};
            if (factory2->getClassInfo2(index, &info2) == Steinberg::kResultOk) {
                if (info2.vendor[0] != 0)
                    vendor = juce::String(info2.vendor).trim();
                if (info2.version[0] != 0)
                    version = juce::String(info2.version).trim();
                if (info2.subCategories[0] != 0)
                    category = juce::String(info2.subCategories).trim();
            }
        }

        juce::PluginDescription description;
        description.fileOrIdentifier = path;
        description.lastFileModTime = binary.getLastModificationTime();
        description.lastInfoUpdateTime = juce::Time::getCurrentTime();
        description.manufacturerName = vendor;
        description.name = name;
        description.descriptiveName = name;
        description.pluginFormatName = "VST3";
        description.numInputChannels = 0;
        description.numOutputChannels = 0;
        description.uniqueId = hashNormalised(info.cid);
        description.deprecatedUid = hashRaw(info.cid);
        description.category = category;
        description.isInstrument = category.containsIgnoreCase("Instrument");
        description.version = version;
        result.push_back(std::move(description));
    }

    if (factory2 != nullptr)
        factory2->release();
    factory->release();

    if (auto* exit = reinterpret_cast<EntryProc>(library.getFunction("ExitDll")))
        exit();
    library.close();
    return result;
}

juce::AudioProcessor* createRaw(juce::AudioPluginFormat& format,
                                const juce::PluginDescription& description,
                                double sampleRate,
                                int blockSize,
                                juce::String& error)
{
    auto created = format.createInstanceFromDescription(description, sampleRate, blockSize, error);
    return created.release();
}

int32_t guardedCreate(juce::AudioPluginFormat& format,
                      const juce::PluginDescription& description,
                      double sampleRate,
                      int blockSize,
                      juce::String& error,
                      juce::AudioProcessor** out)
{
    *out = nullptr;
#if defined(_MSC_VER)
    __try {
        *out = createRaw(format, description, sampleRate, blockSize, error);
        return *out != nullptr ? MH_OK : MH_ERR;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        *out = nullptr;
        return MH_ERR;
    }
#else
    *out = createRaw(format, description, sampleRate, blockSize, error);
    return *out != nullptr ? MH_OK : MH_ERR;
#endif
}

void getStateRaw(juce::AudioProcessor& processor, juce::MemoryBlock& block)
{
    processor.getStateInformation(block);
}

int32_t guardedGetState(juce::AudioProcessor& processor, juce::MemoryBlock& block)
{
#if defined(_MSC_VER)
    __try {
        getStateRaw(processor, block);
        return MH_OK;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return MH_ERR;
    }
#else
    getStateRaw(processor, block);
    return MH_OK;
#endif
}

void setStateRaw(juce::AudioProcessor& processor, const void* data, int size)
{
    processor.setStateInformation(data, size);
}

int32_t guardedSetState(juce::AudioProcessor& processor, const void* data, int size)
{
#if defined(_MSC_VER)
    __try {
        setStateRaw(processor, data, size);
        return MH_OK;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return MH_ERR;
    }
#else
    setStateRaw(processor, data, size);
    return MH_OK;
#endif
}

juce::AudioChannelSet channelSetFor(int channels)
{
    if (channels <= 0)
        return juce::AudioChannelSet::disabled();
    return juce::AudioChannelSet::canonicalChannelSet(channels);
}

int auxiliaryInputChannels(juce::AudioProcessor& processor)
{
    int total = 0;
    for (int index = 1; index < processor.getBusCount(true); ++index)
        if (auto* bus = processor.getBus(true, index))
            total += juce::jmax(0, bus->getDefaultLayout().size());
    return total;
}

struct LoadResult {
    int32_t status = MH_ERR;
    PluginInstance* instance = nullptr;
    juce::String error;
};

struct PrepareResult {
    int32_t status = MH_OK;
    juce::String message;
};

PluginInstance* instanceOf(void* handle)
{
    return static_cast<PluginInstance*>(handle);
}

juce::String classIdOf(const juce::PluginDescription& description)
{
    return juce::String(description.uniqueId);
}

}

namespace michost {

void Host::destroyPlugins()
{
    std::vector<PluginInstance*> pending;
    {
        const std::lock_guard<std::mutex> lock(pluginMutex);
        pending.swap(plugins);
    }

    for (auto* instance : pending) {
        if (instance == nullptr)
            continue;
        closePluginEditor(*instance);
        delete instance;
    }
}

}

extern "C" {

int32_t mh_plugin_describe(void* manager,
                           const char* path,
                           MHPluginDescriptor* out,
                           int32_t max,
                           int32_t* count)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || path == nullptr || count == nullptr || max < 0 || (max > 0 && out == nullptr))
        return MH_INVALID_ARGUMENT;
    *count = 0;

    try {
        prepareCom();
        auto* format = findVst3(*host);
        if (format == nullptr)
            return MH_NOT_SUPPORTED;
        if (host->gui == nullptr)
            return MH_ERR;

        const auto descriptions = enumerateModule(toJuce(path));
        if (descriptions.empty()) {
            reportError(*host, MH_ERR, "reading the plugin module failed");
            return MH_ERR;
        }

        std::vector<const juce::PluginDescription*> classes;
        juce::StringArray seen;
        for (const auto& description : descriptions) {
            const auto key = classIdOf(description) + "|" + description.name;
            if (seen.contains(key))
                continue;
            seen.add(key);
            classes.push_back(&description);
        }

        const auto total = static_cast<int32_t>(std::min<size_t>(classes.size(), 0x7fffffff));
        const auto limit = static_cast<size_t>(juce::jmin(total, max));
        for (size_t index = 0; index < limit; ++index) {
            auto& descriptor = out[index];
            std::memset(&descriptor, 0, sizeof(descriptor));
            writeString(descriptor.path, toJuce(path));
            writeString(descriptor.class_id, classIdOf(*classes[index]));
            writeString(descriptor.name, classes[index]->name);
            writeString(descriptor.vendor, classes[index]->manufacturerName);
            writeString(descriptor.category, classes[index]->category);

            descriptor.main_inputs = static_cast<uint32_t>(juce::jmax(0, classes[index]->numInputChannels));
            descriptor.main_outputs = static_cast<uint32_t>(juce::jmax(0, classes[index]->numOutputChannels));
            descriptor.sidechain_inputs = 0;
        }

        *count = total;
        return MH_OK;
    } catch (...) {
        reportError(*host, MH_ERR, "describing the plugin module failed");
        return MH_ERR;
    }
}

static int32_t loadPlugin(void* manager,
                          const char* path,
                          const char* class_id,
                          const char* name,
                          void** out_handle)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || path == nullptr || out_handle == nullptr || host->gui == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_handle = nullptr;

    LoadResult result;
    try {
        prepareCom();
        result = host->gui->callSync([&]() -> LoadResult {
            LoadResult loaded;
            auto* format = findVst3(*host);
            if (format == nullptr) {
                loaded.error = "VST3 support is not available in this build";
                return loaded;
            }

            const auto wanted = toJuce(class_id);
            const auto requestedName = toJuce(name);
            juce::PluginDescription fastDescription;
            juce::PluginDescription* match = nullptr;
            juce::String classIdOut;

            if (requestedName.isNotEmpty() && wanted.isNotEmpty()) {
                fastDescription.fileOrIdentifier = toJuce(path);
                fastDescription.name = requestedName;
                fastDescription.pluginFormatName = "VST3";
                fastDescription.uniqueId = wanted.getIntValue();
                fastDescription.deprecatedUid = fastDescription.uniqueId;
                match = &fastDescription;
                classIdOut = wanted;
            } else {
                auto descriptions = enumerateModule(toJuce(path));
                if (descriptions.empty()) {
                    loaded.error = "the plugin module exposes no classes";
                    return loaded;
                }
                if (wanted.isNotEmpty()) {
                    for (auto& description : descriptions) {
                        if (classIdOf(description) == wanted) {
                            fastDescription = description;
                            match = &fastDescription;
                            break;
                        }
                    }
                }
                if (match == nullptr) {
                    fastDescription = descriptions[0];
                    match = &fastDescription;
                }
                classIdOut = classIdOf(*match);
            }

            juce::String error;
            juce::AudioProcessor* raw = nullptr;
            const auto created = guardedCreate(*format, *match, 48000.0, 256, error, &raw);
            if (created != MH_OK || raw == nullptr) {
                loaded.error = error.isNotEmpty() ? error : "the plugin could not be instantiated";
                return loaded;
            }

            std::unique_ptr<juce::AudioProcessor> owner(raw);
            auto instance = std::make_unique<PluginInstance>();
            instance->processor = std::move(owner);
            instance->path = toJuce(path);
            instance->classId = classIdOut;
            instance->host = host;
            instance->editListener =
                std::make_unique<PluginChangeListener>(instance->edited, instance->editorOpen);
            instance->processor->addListener(instance->editListener.get());
            loaded.instance = instance.release();
            loaded.status = MH_OK;
            return loaded;
        });
    } catch (...) {
        result.status = MH_ERR;
        result.error = "the plugin could not be instantiated";
    }

    if (result.status != MH_OK) {
        reportError(*host, result.status, result.error);
        return result.status;
    }

    try {
        const std::lock_guard<std::mutex> lock(host->pluginMutex);
        host->plugins.push_back(result.instance);
    } catch (...) {
        auto* instance = result.instance;
        try {
            host->gui->callSync([instance] { delete instance; });
        } catch (...) {
        }
        return MH_ERR;
    }

    *out_handle = result.instance;
    return MH_OK;
}

int32_t mh_plugin_take_edited(void* handle)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr)
        return 0;
    return instance->edited.exchange(false, std::memory_order_acq_rel) ? 1 : 0;
}

int32_t mh_plugin_load(void* manager, const char* path, const char* class_id, void** out_handle)
{
    return loadPlugin(manager, path, class_id, nullptr, out_handle);
}

int32_t mh_plugin_load_named(void* manager,
                             const char* path,
                             const char* class_id,
                             const char* name,
                             void** out_handle)
{
    return loadPlugin(manager, path, class_id, name, out_handle);
}

int32_t mh_plugin_free(void* manager, void* handle)
{
    auto* host = static_cast<Host*>(manager);
    auto* instance = instanceOf(handle);
    if (host == nullptr || instance == nullptr)
        return MH_INVALID_ARGUMENT;

    try {
        bool found = false;
        {
            const std::lock_guard<std::mutex> lock(host->pluginMutex);
            auto& instances = host->plugins;
            for (auto iterator = instances.begin(); iterator != instances.end(); ++iterator) {
                if (*iterator == instance) {
                    instances.erase(iterator);
                    found = true;
                    break;
                }
            }
        }
        if (!found)
            return MH_INVALID_ARGUMENT;

        if (host->gui != nullptr)
            host->gui->callSync([instance] {
                closePluginEditor(*instance);
                delete instance;
            });
        else {
            closePluginEditor(*instance);
            delete instance;
        }
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_channels(void* handle,
                           int32_t* out_main_inputs,
                           int32_t* out_main_outputs,
                           int32_t* out_sidechain_inputs)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out_main_inputs == nullptr || out_main_outputs == nullptr
        || out_sidechain_inputs == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_main_inputs = 0;
    *out_main_outputs = 0;
    *out_sidechain_inputs = 0;

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, out_main_inputs, out_main_outputs, out_sidechain_inputs]() -> int32_t {
                auto& processor = *instance->processor;
                *out_main_inputs = juce::jmax(0, processor.getMainBusNumInputChannels());
                *out_main_outputs = juce::jmax(0, processor.getMainBusNumOutputChannels());
                *out_sidechain_inputs = auxiliaryInputChannels(processor);
                return MH_OK;
            });

        auto& processor = *instance->processor;
        *out_main_inputs = juce::jmax(0, processor.getMainBusNumInputChannels());
        *out_main_outputs = juce::jmax(0, processor.getMainBusNumOutputChannels());
        *out_sidechain_inputs = auxiliaryInputChannels(processor);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_prepare(void* manager,
                          void* handle,
                          double sample_rate,
                          int32_t max_block,
                          int32_t main_inputs,
                          int32_t main_outputs,
                          int32_t sidechain_channels)
{
    auto* host = static_cast<Host*>(manager);
    auto* instance = instanceOf(handle);
    if (host == nullptr || instance == nullptr)
        return MH_INVALID_ARGUMENT;
    if (main_inputs < 0 || main_outputs < 0 || sidechain_channels < 0)
        return MH_INVALID_ARGUMENT;
    if (host->gui == nullptr)
        return MH_ERR;

    try {
        const auto result = host->gui->callSync([&]() -> PrepareResult {
            PrepareResult prepared;
            auto& processor = *instance->processor;

            instance->prepared.store(false, std::memory_order_release);
            instance->active.store(false, std::memory_order_release);
            instance->resourcesReleased.store(false, std::memory_order_release);
            instance->sampleRate = sample_rate > 0.0 ? sample_rate : 48000.0;
            instance->blockSize = max_block > 0 ? max_block : 256;

            auto layout = processor.getBusesLayout();
            if (main_inputs > 0 && layout.inputBuses.isEmpty()) {
                prepared.status = MH_ERR;
                prepared.message = "the plugin has no main input bus but " + juce::String(main_inputs)
                                   + " input channels were requested";
                return prepared;
            }
            if (main_outputs > 0 && layout.outputBuses.isEmpty()) {
                prepared.status = MH_ERR;
                prepared.message = "the plugin has no main output bus but " + juce::String(main_outputs)
                                   + " output channels were requested";
                return prepared;
            }

            if (!layout.inputBuses.isEmpty())
                layout.inputBuses.set(0, channelSetFor(main_inputs));
            if (!layout.outputBuses.isEmpty())
                layout.outputBuses.set(0, channelSetFor(main_outputs));

            if (sidechain_channels > 0) {
                const int auxCount = juce::jmax(0, layout.inputBuses.size() - 1);
                std::vector<juce::AudioChannelSet> auxLayouts(static_cast<size_t>(auxCount));
                std::vector<int> auxSizes(static_cast<size_t>(auxCount), 0);
                for (int index = 0; index < auxCount; ++index) {
                    auto* bus = processor.getBus(true, index + 1);
                    auto forBus = bus != nullptr ? bus->getDefaultLayout()
                                                 : juce::AudioChannelSet::disabled();
                    if (forBus.isDisabled() || forBus.size() <= 0)
                        forBus = juce::AudioChannelSet::disabled();
                    auxLayouts[static_cast<size_t>(index)] = forBus;
                    auxSizes[static_cast<size_t>(index)] = forBus.isDisabled() ? 0 : forBus.size();
                }

                std::vector<int> reach(static_cast<size_t>(sidechain_channels) + 1, -1);
                reach[0] = -2;
                for (int busIndex = 0; busIndex < auxCount; ++busIndex) {
                    const auto size = auxSizes[static_cast<size_t>(busIndex)];
                    if (size <= 0)
                        continue;
                    for (int sum = sidechain_channels; sum >= size; --sum)
                        if (reach[static_cast<size_t>(sum)] == -1
                            && reach[static_cast<size_t>(sum - size)] != -1)
                            reach[static_cast<size_t>(sum)] = busIndex;
                }

                if (reach[static_cast<size_t>(sidechain_channels)] == -1) {
                    prepared.status = MH_ERR;
                    prepared.message = "the plugin's aux input buses cannot represent "
                                       + juce::String(sidechain_channels) + " sidechain channels";
                    return prepared;
                }

                std::vector<char> chosen(static_cast<size_t>(auxCount), 0);
                for (int sum = sidechain_channels; sum > 0;) {
                    const auto busIndex = reach[static_cast<size_t>(sum)];
                    chosen[static_cast<size_t>(busIndex)] = 1;
                    sum -= auxSizes[static_cast<size_t>(busIndex)];
                }

                for (int index = 0; index < auxCount; ++index) {
                    layout.inputBuses.set(index + 1,
                                          chosen[static_cast<size_t>(index)] != 0
                                              ? auxLayouts[static_cast<size_t>(index)]
                                              : juce::AudioChannelSet::disabled());
                }
            } else {
                for (int index = 1; index < layout.inputBuses.size(); ++index)
                    layout.inputBuses.set(index, juce::AudioChannelSet::disabled());
            }

            for (int index = 1; index < layout.outputBuses.size(); ++index)
                layout.outputBuses.set(index, juce::AudioChannelSet::disabled());

            if (!processor.setBusesLayout(layout)) {
                prepared.status = MH_ERR;
                prepared.message = "the plugin rejected the requested bus layout";
                return prepared;
            }

            processor.setRateAndBufferSizeDetails(instance->sampleRate, instance->blockSize);
            processor.prepareToPlay(instance->sampleRate, instance->blockSize);

            instance->totalInputChannels.store(juce::jmax(0, processor.getTotalNumInputChannels()),
                                               std::memory_order_release);
            instance->totalOutputChannels.store(juce::jmax(0, processor.getTotalNumOutputChannels()),
                                                std::memory_order_release);
            instance->mainInputChannels.store(juce::jmax(0, processor.getMainBusNumInputChannels()),
                                              std::memory_order_release);
            instance->mainOutputChannels.store(juce::jmax(0, processor.getMainBusNumOutputChannels()),
                                               std::memory_order_release);

            const auto required = juce::jmax(instance->totalInputChannels.load(std::memory_order_acquire),
                                             instance->totalOutputChannels.load(std::memory_order_acquire));
            instance->processFrames = required > 0 ? juce::jmax(instance->blockSize, maxProcessFrames) : 0;
            instance->processBuffer.setSize(required, instance->processFrames, false, true, false);
            instance->prepared.store(true, std::memory_order_release);
            return prepared;
        });

        if (result.status != MH_OK) {
            reportError(*host, result.status, result.message);
            return result.status;
        }

        reportLatency(*host, *instance);
        return MH_OK;
    } catch (...) {
        reportError(*host, MH_ERR, "preparing the plugin failed");
        return MH_ERR;
    }
}

int32_t mh_plugin_activate(void* manager, void* handle, int32_t active)
{
    auto* host = static_cast<Host*>(manager);
    auto* instance = instanceOf(handle);
    if (host == nullptr || instance == nullptr)
        return MH_INVALID_ARGUMENT;
    if (host->gui == nullptr)
        return MH_ERR;

    try {
        const auto status = host->gui->callSync([instance, active]() -> int32_t {
            auto& processor = *instance->processor;
            if (active != 0) {
                if (!instance->prepared.load(std::memory_order_acquire))
                    return MH_ERR;
                if (!instance->active.exchange(true, std::memory_order_acq_rel)
                    && instance->resourcesReleased.exchange(false, std::memory_order_acq_rel))
                    processor.prepareToPlay(instance->sampleRate, instance->blockSize);
            } else {
                if (instance->active.exchange(false, std::memory_order_acq_rel)) {
                    processor.releaseResources();
                    instance->resourcesReleased.store(true, std::memory_order_release);
                }
            }
            return MH_OK;
        });

        if (status != MH_OK) {
            reportError(*host, status, "the plugin must be prepared before it can be activated");
            return status;
        }

        reportLatency(*host, *instance);
        return MH_OK;
    } catch (...) {
        reportError(*host, MH_ERR, "activating the plugin failed");
        return MH_ERR;
    }
}

int32_t mh_plugin_process(void* handle, float* const* channels, int32_t num_channels, int32_t frames)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || frames < 0)
        return MH_INVALID_ARGUMENT;

    try {
        if (!instance->prepared.load(std::memory_order_acquire))
            return MH_ERR;
        if (!instance->active.load(std::memory_order_acquire))
            return MH_OK;
        if (channels == nullptr && num_channels > 0)
            return MH_INVALID_ARGUMENT;

        const auto required = juce::jmax(instance->totalInputChannels.load(std::memory_order_acquire),
                                         instance->totalOutputChannels.load(std::memory_order_acquire));
        if (num_channels < required)
            return MH_ERR;
        if (frames == 0 || required == 0)
            return MH_OK;
        if (required > instance->processBuffer.getNumChannels() || frames > instance->processFrames)
            return MH_ERR;
        for (int channel = 0; channel < required; ++channel)
            if (channels[channel] == nullptr)
                return MH_INVALID_ARGUMENT;

        juce::ScopedNoDenormals noDenormals;
        auto& buffer = instance->processBuffer;
        buffer.setSize(required, frames, false, false, true);
        for (int channel = 0; channel < required; ++channel)
            juce::FloatVectorOperations::copy(buffer.getWritePointer(channel), channels[channel], frames);

        juce::MidiBuffer midi;
        instance->processor->processBlock(buffer, midi);

        for (int channel = 0; channel < required; ++channel)
            juce::FloatVectorOperations::copy(channels[channel], buffer.getReadPointer(channel), frames);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_latency(void* handle, int32_t* out_samples)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out_samples == nullptr)
        return MH_INVALID_ARGUMENT;
    try {
        *out_samples = juce::jmax(0, instance->processor->getLatencySamples());
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_param_count(void* handle, int32_t* out_count)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out_count == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_count = 0;

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, out_count]() -> int32_t {
                *out_count = instance->processor->getParameters().size();
                return MH_OK;
            });

        *out_count = instance->processor->getParameters().size();
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_param_info(void* handle, int32_t index, MHParamInfo* out)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out == nullptr)
        return MH_INVALID_ARGUMENT;
    std::memset(out, 0, sizeof(*out));

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, index, out]() -> int32_t {
                const auto& parameters = instance->processor->getParameters();
                if (index < 0 || index >= parameters.size())
                    return MH_INVALID_ARGUMENT;

                auto* parameter = parameters[index];
                out->id = static_cast<uint32_t>(index);
                writeString(out->name, parameter->getName(static_cast<int>(sizeof(out->name))));
                writeString(out->units, parameter->getLabel());
                out->default_value = parameter->getDefaultValue();
                out->step_count = static_cast<uint32_t>(juce::jmax(0, parameter->getNumSteps()));
                return MH_OK;
            });

        const auto& parameters = instance->processor->getParameters();
        if (index < 0 || index >= parameters.size())
            return MH_INVALID_ARGUMENT;

        auto* parameter = parameters[index];
        out->id = static_cast<uint32_t>(index);
        writeString(out->name, parameter->getName(static_cast<int>(sizeof(out->name))));
        writeString(out->units, parameter->getLabel());
        out->default_value = parameter->getDefaultValue();
        out->step_count = static_cast<uint32_t>(juce::jmax(0, parameter->getNumSteps()));
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_param_set(void* handle, uint32_t id, double normalized)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr)
        return MH_INVALID_ARGUMENT;

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, id, normalized]() -> int32_t {
                const auto& parameters = instance->processor->getParameters();
                if (id >= static_cast<uint32_t>(parameters.size()))
                    return MH_INVALID_ARGUMENT;
                parameters[static_cast<int>(id)]->setValueNotifyingHost(
                    static_cast<float>(juce::jlimit(0.0, 1.0, normalized)));
                return MH_OK;
            });

        const auto& parameters = instance->processor->getParameters();
        if (id >= static_cast<uint32_t>(parameters.size()))
            return MH_INVALID_ARGUMENT;
        parameters[static_cast<int>(id)]->setValueNotifyingHost(
            static_cast<float>(juce::jlimit(0.0, 1.0, normalized)));
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_param_get(void* handle, uint32_t id, double* out_value)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out_value == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_value = 0.0;

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, id, out_value]() -> int32_t {
                const auto& parameters = instance->processor->getParameters();
                if (id >= static_cast<uint32_t>(parameters.size()))
                    return MH_INVALID_ARGUMENT;
                *out_value = parameters[static_cast<int>(id)]->getValue();
                return MH_OK;
            });

        const auto& parameters = instance->processor->getParameters();
        if (id >= static_cast<uint32_t>(parameters.size()))
            return MH_INVALID_ARGUMENT;
        *out_value = parameters[static_cast<int>(id)]->getValue();
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_state_size(void* handle, int32_t kind, int32_t* out_size)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || out_size == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_size = 0;
    if (kind != MH_STATE_COMPONENT)
        return MH_NOT_SUPPORTED;

    try {
        auto* host = instance->host;
        const auto read = [instance, out_size]() -> int32_t {
            juce::MemoryBlock block;
            const auto status = guardedGetState(*instance->processor, block);
            if (status == MH_OK)
                *out_size = static_cast<int32_t>(juce::jmin<size_t>(block.getSize(), 0x7fffffff));
            return status;
        };

        int32_t status = MH_OK;
        if (host != nullptr && host->gui != nullptr)
            status = host->gui->callSync(read);
        else
            status = read();

        return status;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_state_export(void* handle,
                               int32_t kind,
                               uint8_t* out,
                               int32_t capacity,
                               int32_t* written)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || written == nullptr || capacity < 0)
        return MH_INVALID_ARGUMENT;
    *written = 0;
    if (kind != MH_STATE_COMPONENT)
        return MH_NOT_SUPPORTED;

    try {
        auto* host = instance->host;
        const auto exportState = [instance, out, capacity, written]() -> int32_t {
            juce::MemoryBlock block;
            const auto status = guardedGetState(*instance->processor, block);
            if (status != MH_OK)
                return status;

            const auto size = static_cast<int32_t>(juce::jmin<size_t>(block.getSize(), 0x7fffffff));
            if (capacity < size || (size > 0 && out == nullptr))
                return MH_INVALID_ARGUMENT;
            if (size > 0)
                std::memcpy(out, block.getData(), static_cast<size_t>(size));
            *written = size;
            return MH_OK;
        };

        int32_t status = MH_OK;
        if (host != nullptr && host->gui != nullptr)
            status = host->gui->callSync(exportState);
        else
            status = exportState();

        return status;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_state_import(void* handle, int32_t kind, const uint8_t* data, int32_t len)
{
    auto* instance = instanceOf(handle);
    if (instance == nullptr || len < 0 || (len > 0 && data == nullptr))
        return MH_INVALID_ARGUMENT;
    if (kind != MH_STATE_COMPONENT)
        return MH_NOT_SUPPORTED;
    if (len == 0)
        return MH_OK;

    try {
        auto* host = instance->host;
        const auto importState = [instance, data, len]() -> int32_t {
            return guardedSetState(*instance->processor, data, len);
        };

        int32_t status = MH_OK;
        if (host != nullptr && host->gui != nullptr)
            status = host->gui->callSync(importState);
        else
            status = importState();

        if (status != MH_OK)
            return status;

        if (host != nullptr)
            reportLatency(*host, *instance);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

}
