#include "host_internal.h"

using namespace michost;

namespace {

juce::AudioPluginFormat* findVst3(Host& host)
{
    for (auto* format : host.formats.getFormats())
        if (format != nullptr && format->getName().equalsIgnoreCase("VST3"))
            return format;
    return nullptr;
}

bool samePath(const juce::File& left, const juce::File& right)
{
    return left.getFullPathName().equalsIgnoreCase(right.getFullPathName());
}

juce::Array<juce::File> searchPaths()
{
    juce::Array<juce::File> paths;

    const auto common = juce::SystemStats::getEnvironmentVariable(
        "COMMONPROGRAMFILES", "C:\\Program Files\\Common Files");
    paths.add(juce::File(common).getChildFile("VST3"));

    const auto local = juce::SystemStats::getEnvironmentVariable("LOCALAPPDATA", {});
    if (local.isNotEmpty()) {
        const auto localRoot = juce::File(local);
        paths.add(localRoot.getChildFile("Programs").getChildFile("Common").getChildFile("VST3"));
        paths.add(localRoot.getChildFile("Programs").getChildFile("VST3"));
        paths.add(localRoot.getChildFile("VST3"));
    }

    const auto programFiles = juce::SystemStats::getEnvironmentVariable("PROGRAMFILES", {});
    if (programFiles.isNotEmpty())
        paths.add(juce::File(programFiles).getChildFile("VST3"));

    const auto appData = juce::SystemStats::getEnvironmentVariable("APPDATA", {});
    if (appData.isNotEmpty())
        paths.add(juce::File(appData).getChildFile("VST3"));

    const auto userProfile = juce::SystemStats::getEnvironmentVariable("USERPROFILE", {});
    if (userProfile.isNotEmpty())
        paths.add(juce::File(userProfile).getChildFile("VST3"));

    const auto programData = juce::SystemStats::getEnvironmentVariable("PROGRAMDATA", {});
    if (programData.isNotEmpty())
        paths.add(juce::File(programData).getChildFile("VST3"));

    const auto extra = juce::SystemStats::getEnvironmentVariable("VST3_PATH", {});
    for (const auto& token : juce::StringArray::fromTokens(extra, ";", {}))
        if (token.isNotEmpty())
            paths.add(juce::File(token));

    return paths;
}

void scanDirectory(const juce::File& directory, juce::Array<juce::File>& files)
{
    for (juce::RangedDirectoryIterator iterator(
             directory, false, "*", juce::File::findFilesAndDirectories);
         iterator != juce::RangedDirectoryIterator();
         ++iterator) {
        const auto file = iterator->getFile();
        if (file.getFileName().endsWithIgnoreCase(".vst3")) {
            if (!files.contains(file))
                files.add(file);
        } else if (file.isDirectory()) {
            scanDirectory(file, files);
        }
    }
}

void collectBundles(const juce::File& root, juce::Array<juce::File>& files)
{
    if (!root.exists())
        return;

    if (root.getFileName().endsWithIgnoreCase(".vst3")) {
        if (!files.contains(root))
            files.add(root);
        return;
    }

    if (root.isDirectory())
        scanDirectory(root, files);
}

juce::Array<juce::File> collectAllBundles(const juce::String& extraPaths)
{
    juce::StringArray includes;
    juce::StringArray excludes;
    for (const auto& token : juce::StringArray::fromTokens(extraPaths, ";", {})) {
        if (token.isEmpty())
            continue;
        if (token.startsWithChar('!'))
            excludes.add(token.substring(1));
        else
            includes.add(token);
    }

    juce::Array<juce::File> roots;
    for (const auto& directory : searchPaths())
        roots.addIfNotAlreadyThere(directory);

    for (int index = roots.size(); --index >= 0;) {
        bool excluded = false;
        for (const auto& path : excludes) {
            if (samePath(roots[index], juce::File(path))) {
                excluded = true;
                break;
            }
        }
        if (excluded)
            roots.remove(index);
    }

    for (const auto& path : includes)
        if (path.isNotEmpty())
            roots.addIfNotAlreadyThere(juce::File(path));

    juce::Array<juce::File> files;
    for (const auto& directory : roots)
        collectBundles(directory, files);
    return files;
}

int32_t scanBundles(Host& host,
                    const juce::String& extraPaths,
                    MHScanProgress progress,
                    void* progress_ctx,
                    MHPluginDescriptor* out,
                    int32_t max,
                    int32_t* count)
{
    *count = 0;
    try {
        if (findVst3(host) == nullptr)
            return MH_NOT_SUPPORTED;

        const auto files = collectAllBundles(extraPaths);
        const auto total = static_cast<int32_t>(std::min<size_t>(files.size(), 0x7fffffff));
        int32_t written = 0;
        int32_t done = 0;

        for (const auto& file : files) {
            ++done;

            MHPluginDescriptor descriptor{};
            writeString(descriptor.path, file.getFullPathName());
            writeString(descriptor.name, file.getFileNameWithoutExtension());
            descriptor.main_inputs = 2;
            descriptor.main_outputs = 2;

            if (written < max) {
                out[written] = descriptor;
                ++written;
            }

            if (progress != nullptr)
                progress(progress_ctx, &descriptor, done, total);
        }

        *count = total;
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

}

extern "C" {

int32_t mh_scan(void* manager,
                MHScanProgress progress,
                void* progress_ctx,
                MHPluginDescriptor* out,
                int32_t max,
                int32_t* count)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || count == nullptr || max < 0 || (max > 0 && out == nullptr))
        return MH_INVALID_ARGUMENT;
    return scanBundles(*host, {}, progress, progress_ctx, out, max, count);
}

int32_t mh_scan_paths(void* manager,
                      const char* extra_paths,
                      MHScanProgress progress,
                      void* progress_ctx,
                      MHPluginDescriptor* out,
                      int32_t max,
                      int32_t* count)
{
    auto* host = static_cast<Host*>(manager);
    if (host == nullptr || count == nullptr || max < 0 || (max > 0 && out == nullptr))
        return MH_INVALID_ARGUMENT;
    const auto extra = extra_paths != nullptr ? juce::String::fromUTF8(extra_paths) : juce::String();
    return scanBundles(*host, extra, progress, progress_ctx, out, max, count);
}

}
