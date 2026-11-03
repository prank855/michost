#include "host_internal.h"

#if JUCE_WINDOWS
#include <windows.h>
#endif

using namespace michost;

namespace michost {

void GuiThread::post(std::function<void()> task)
{
    {
        const std::lock_guard<std::mutex> lock(mutex);
        tasks.push_back(std::move(task));
    }
    notify();
}

void GuiThread::drain()
{
    for (;;) {
        std::function<void()> task;
        {
            const std::lock_guard<std::mutex> lock(mutex);
            if (tasks.empty())
                return;
            task = std::move(tasks.front());
            tasks.pop_front();
        }
        task();
    }
}

void GuiThread::run()
{
    prepareComGui();
    auto* manager = juce::MessageManager::getInstance();
    running.store(true, std::memory_order_release);

    while (!threadShouldExit()) {
        manager->runDispatchLoopUntil(5);
        drain();
        wait(1);
    }

    drain();
    running.store(false, std::memory_order_release);
    juce::MessageManager::deleteInstance();
}

void GuiThread::stop()
{
    signalThreadShouldExit();
    notify();
    stopThread(5000);
}

juce::Image makeAppIcon(const int size)
{
    juce::Image image(juce::Image::ARGB, size, size, true);
    const auto scale = static_cast<float>(size) / 32.0f;

    juce::Graphics graphics(image);
    graphics.setImageResamplingQuality(juce::Graphics::highResamplingQuality);
    graphics.setColour(juce::Colour(0xff3355b5));
    graphics.fillRoundedRectangle(0.0f, 0.0f, static_cast<float>(size), static_cast<float>(size), 7.0f * scale);
    graphics.setColour(juce::Colour(0xffeef2fa));
    graphics.fillRoundedRectangle(12.0f * scale,
                                  4.0f * scale,
                                  8.0f * scale,
                                  15.0f * scale,
                                  4.0f * scale);
    graphics.setColour(juce::Colour(0xffeef2fa));
    graphics.fillRect(14.9f * scale, 19.0f * scale, 2.2f * scale, 7.0f * scale);
    graphics.fillRoundedRectangle(10.5f * scale,
                                  24.6f * scale,
                                  11.0f * scale,
                                  2.8f * scale,
                                  1.4f * scale);

    juce::Path arc;
    arc.addCentredArc(16.0f * scale,
                      17.0f * scale,
                      8.0f * scale,
                      8.0f * scale,
                      0.0f,
                      juce::MathConstants<float>::halfPi,
                      juce::MathConstants<float>::halfPi * 3.0f,
                      true);
    graphics.strokePath(arc, juce::PathStrokeType(1.4f * scale));
    return image;
}

class PluginEditorWindow final : public juce::DocumentWindow {
public:
    PluginEditorWindow(const juce::String& title, std::atomic<bool>& editorOpen)
        : juce::DocumentWindow(title, juce::Colour(0xff20242c), juce::DocumentWindow::closeButton),
          editorOpen(editorOpen)
    {
        setUsingNativeTitleBar(true);
        setIcon(makeAppIcon(32));
        setResizable(true, true);
        setAlwaysOnTop(true);
    }

    ~PluginEditorWindow() override { editorOpen.store(false, std::memory_order_release); }

    void closeButtonPressed() override
    {
        editorOpen.store(false, std::memory_order_release);
        setVisible(false);
    }

    void visibilityChanged() override
    {
        editorOpen.store(isVisible(), std::memory_order_release);
        if (isVisible())
            if (auto* peer = getPeer())
                peer->setIcon(makeAppIcon(32));
    }

private:
    std::atomic<bool>& editorOpen;
};

namespace {

void raiseNativeWindow(juce::Component& component)
{
#if JUCE_WINDOWS
    auto* peer = component.getPeer();
    if (peer == nullptr)
        return;
    auto window = reinterpret_cast<HWND>(peer->getNativeHandle());
    if (window == nullptr)
        return;

    ShowWindow(window, SW_RESTORE);
    SetWindowPos(window,
                 HWND_TOPMOST,
                 0,
                 0,
                 0,
                 0,
                 SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
    BringWindowToTop(window);
    SetForegroundWindow(window);
    SetActiveWindow(window);
#else
    juce::ignoreUnused(component);
#endif
}

void bringEditorToFront(PluginEditorWindow& window)
{
    window.setVisible(true);
    window.setAlwaysOnTop(true);
    window.toFront(true);
    window.grabKeyboardFocus();
    raiseNativeWindow(window);

    juce::Component::SafePointer<PluginEditorWindow> safe(&window);
    juce::Timer::callAfterDelay(50, [safe]() mutable {
        if (auto* target = safe.getComponent()) {
            target->setAlwaysOnTop(true);
            target->toFront(true);
            target->grabKeyboardFocus();
            raiseNativeWindow(*target);
        }
    });
}

}

void closePluginEditor(PluginInstance& instance)
{
    instance.editorOpen.store(false, std::memory_order_release);
    if (instance.editorWindow != nullptr) {
        instance.editorWindow->clearContentComponent();
        instance.editorWindow.reset();
    }
    instance.editor.reset();
}

}

extern "C" {

int32_t mh_plugin_editor_open(void* manager, void* handle)
{
    auto* host = static_cast<Host*>(manager);
    auto* instance = static_cast<PluginInstance*>(handle);
    if (host == nullptr || instance == nullptr || host->gui == nullptr)
        return MH_INVALID_ARGUMENT;

    try {
        return host->gui->callSync([host, instance]() -> int32_t {
            if (instance->editor != nullptr) {
                if (instance->editorWindow != nullptr) {
                    if (auto* window = dynamic_cast<PluginEditorWindow*>(instance->editorWindow.get())) {
                        bringEditorToFront(*window);
                    } else {
                        instance->editorWindow->setVisible(true);
                        instance->editorWindow->toFront(true);
                    }
                } else {
                    instance->editor->setVisible(true);
                }
                instance->editorOpen.store(true, std::memory_order_release);
                return MH_OK;
            }

            auto* editor = instance->processor->createEditorIfNeeded();
            if (editor == nullptr)
                return MH_NOT_SUPPORTED;

            instance->editor.reset(editor);
            auto window = std::make_unique<PluginEditorWindow>(instance->processor->getName(),
                                                              instance->editorOpen);
            window->setContentNonOwned(instance->editor.get(), true);
            window->centreWithSize(juce::jmax(1, editor->getWidth()), juce::jmax(1, editor->getHeight()));
            auto* raw = window.get();
            instance->editorWindow = std::move(window);
            instance->editorOpen.store(true, std::memory_order_release);
            bringEditorToFront(*raw);
            if (auto* peer = raw->getPeer())
                peer->setIcon(makeAppIcon(32));
            return MH_OK;
        });
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_editor_close(void* manager, void* handle)
{
    auto* host = static_cast<Host*>(manager);
    auto* instance = static_cast<PluginInstance*>(handle);
    if (host == nullptr || instance == nullptr)
        return MH_INVALID_ARGUMENT;

    try {
        if (host->gui != nullptr)
            return host->gui->callSync([instance]() -> int32_t {
                closePluginEditor(*instance);
                return MH_OK;
            });

        closePluginEditor(*instance);
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

int32_t mh_plugin_editor_is_open(void* handle, int32_t* out_open)
{
    auto* instance = static_cast<PluginInstance*>(handle);
    if (instance == nullptr || out_open == nullptr)
        return MH_INVALID_ARGUMENT;
    *out_open = 0;

    try {
        auto* host = instance->host;
        if (host != nullptr && host->gui != nullptr)
            return host->gui->callSync([instance, out_open]() -> int32_t {
                *out_open = instance->editorWindow != nullptr && instance->editorWindow->isVisible() ? 1 : 0;
                return MH_OK;
            });

        *out_open = instance->editorWindow != nullptr && instance->editorWindow->isVisible() ? 1 : 0;
        return MH_OK;
    } catch (...) {
        return MH_ERR;
    }
}

}
