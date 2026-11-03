#ifndef MIC_HOST_H
#define MIC_HOST_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#  if defined(MH_BUILD_DLL)
#    define MH_API __declspec(dllexport)
#  else
#    define MH_API
#  endif
#elif defined(__GNUC__)
#  define MH_API __attribute__((visibility("default")))
#else
#  define MH_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define MH_ABI_VERSION 3u

#define MH_PATH_LEN 640
#define MH_ID_LEN 128
#define MH_NAME_LEN 128
#define MH_UNITS_LEN 32
#define MH_DEVICE_LEN 256
#define MH_MESSAGE_LEN 512
#define MH_MAX_BUFFER_SIZES 16

enum {
    MH_API_WASAPI_SHARED = 0,
    MH_API_WASAPI_LOW_LATENCY = 1,
    MH_API_WASAPI_EXCLUSIVE = 2,
    MH_API_ASIO = 3
};

enum {
    MH_OK = 0,
    MH_ERR = 1,
    MH_NOT_SUPPORTED = 2,
    MH_INVALID_ARGUMENT = 3,
    MH_ABI_MISMATCH = 4,
    MH_TIMEOUT = 5,
    MH_NOT_FOUND = 6
};

enum {
    MH_STREAM_INPUT = 0,
    MH_STREAM_OUTPUT = 1
};

enum {
    MH_DEVICE_KIND_INPUT = 0,
    MH_DEVICE_KIND_OUTPUT = 1
};

enum {
    MH_STATE_COMPONENT = 0,
    MH_STATE_CONTROLLER = 1
};

enum {
    MH_LAYOUT_DEVICE_INFO = 0,
    MH_LAYOUT_STREAM_CONFIG = 1,
    MH_LAYOUT_STREAM_CALLBACKS = 2,
    MH_LAYOUT_LATENCY = 3,
    MH_LAYOUT_PLUGIN_DESCRIPTOR = 4,
    MH_LAYOUT_PARAM_INFO = 5,
    MH_LAYOUT_CALLBACKS = 6
};

typedef struct MHCallbacks {
    uint32_t abi_version;
    uint32_t size;
    void* ui_ctx;

    void (*on_error)(void* ui_ctx, int32_t status, const char* message);
    void (*on_latency_changed)(void* ui_ctx, void* plugin, int32_t latency_samples);
} MHCallbacks;

typedef struct MHDeviceInfo {
    char name[MH_DEVICE_LEN];
    char id[MH_ID_LEN];
    uint32_t inputs;
    uint32_t outputs;
    double default_sample_rate;
    int32_t api;
    uint8_t is_default;
    uint8_t has_control_panel;
    uint8_t pad[2];
    uint32_t buffer_sizes[MH_MAX_BUFFER_SIZES];
    uint32_t buffer_size_count;
    uint32_t pad2;
} MHDeviceInfo;

typedef struct MHStreamConfig {
    char device[MH_DEVICE_LEN];
    double sample_rate;
    uint32_t block_size;
    uint32_t channels;
    int32_t api;
    int32_t direction;
    uint64_t channel_mask;
    uint32_t output_channels;
    uint64_t output_channel_mask;
} MHStreamConfig;

typedef struct MHStreamCallbacks {
    uint32_t size;
    uint32_t pad;
    void* ctx;
    void (*on_input)(void* ctx, const float* const* channels, int32_t num_channels, int32_t frames, int64_t sample_pos);
    void (*on_output)(void* ctx, float* const* channels, int32_t num_channels, int32_t frames, int64_t sample_pos);
    void (*on_error)(void* ctx, int32_t status, const char* message);
} MHStreamCallbacks;

typedef struct MHLatency {
    uint32_t input;
    uint32_t output;
    uint32_t buffer;
    uint32_t sample_rate;
} MHLatency;

typedef struct MHPluginDescriptor {
    char path[MH_PATH_LEN];
    char class_id[MH_ID_LEN];
    char name[MH_NAME_LEN];
    char vendor[MH_NAME_LEN];
    uint32_t main_inputs;
    uint32_t main_outputs;
    uint32_t sidechain_inputs;
    uint32_t flags;
    char category[MH_NAME_LEN];
} MHPluginDescriptor;

typedef struct MHParamInfo {
    uint32_t id;
    uint32_t pad;
    char name[MH_NAME_LEN];
    char units[MH_UNITS_LEN];
    double default_value;
    uint32_t step_count;
    uint32_t pad2;
} MHParamInfo;

typedef void (*MHScanProgress)(void* ctx,
                               const MHPluginDescriptor* descriptor,
                               int32_t done,
                               int32_t total);

typedef void (*MHDeviceChangedCallback)(void* ctx);

MH_API uint32_t mh_abi_version(void);
MH_API uint32_t mh_layout_size(int32_t kind);

MH_API int32_t mh_init(const MHCallbacks* callbacks, void** out_manager);
MH_API int32_t mh_set_callbacks(void* manager, const MHCallbacks* callbacks);
MH_API int32_t mh_set_device_change_callback(void* manager,
                                             MHDeviceChangedCallback callback,
                                             void* ctx);
MH_API void mh_shutdown(void* manager);

MH_API int32_t mh_device_enumerate(void* manager,
                                  int32_t kind,
                                  MHDeviceInfo* out,
                                  int32_t max,
                                  int32_t* count);

MH_API int32_t mh_stream_open(void* manager,
                              const MHStreamConfig* config,
                              const MHStreamCallbacks* callbacks,
                              void** out_stream);
MH_API int32_t mh_stream_start(void* stream);
MH_API int32_t mh_stream_stop(void* stream);
MH_API int32_t mh_stream_close(void* stream);
MH_API int32_t mh_stream_latency(void* stream, MHLatency* out);
MH_API int32_t mh_stream_control_panel(void* stream);

MH_API int32_t mh_scan(void* manager,
                       MHScanProgress progress,
                       void* progress_ctx,
                       MHPluginDescriptor* out,
                       int32_t max,
                       int32_t* count);
MH_API int32_t mh_scan_paths(void* manager,
                             const char* extra_paths,
                             MHScanProgress progress,
                             void* progress_ctx,
                             MHPluginDescriptor* out,
                             int32_t max,
                             int32_t* count);
MH_API int32_t mh_plugin_describe(void* manager,
                                  const char* path,
                                  MHPluginDescriptor* out,
                                  int32_t max,
                                  int32_t* count);

MH_API int32_t mh_plugin_load(void* manager,
                              const char* path,
                              const char* class_id,
                              void** out_handle);
MH_API int32_t mh_plugin_load_named(void* manager,
                                    const char* path,
                                    const char* class_id,
                                    const char* name,
                                    void** out_handle);
MH_API int32_t mh_plugin_free(void* manager, void* handle);
MH_API int32_t mh_plugin_channels(void* handle,
                                  int32_t* out_main_inputs,
                                  int32_t* out_main_outputs,
                                  int32_t* out_sidechain_inputs);
MH_API int32_t mh_plugin_prepare(void* manager,
                                 void* handle,
                                 double sample_rate,
                                 int32_t max_block,
                                 int32_t main_inputs,
                                 int32_t main_outputs,
                                 int32_t sidechain_channels);
MH_API int32_t mh_plugin_activate(void* manager, void* handle, int32_t active);
MH_API int32_t mh_plugin_process(void* handle,
                                 float* const* channels,
                                 int32_t num_channels,
                                 int32_t frames);
MH_API int32_t mh_plugin_latency(void* handle, int32_t* out_samples);

MH_API int32_t mh_plugin_param_count(void* handle, int32_t* out_count);
MH_API int32_t mh_plugin_param_info(void* handle, int32_t index, MHParamInfo* out);
MH_API int32_t mh_plugin_param_set(void* handle, uint32_t id, double normalized);
MH_API int32_t mh_plugin_param_get(void* handle, uint32_t id, double* out_value);

MH_API int32_t mh_plugin_state_size(void* handle, int32_t kind, int32_t* out_size);
MH_API int32_t mh_plugin_state_export(void* handle,
                                      int32_t kind,
                                      uint8_t* out,
                                      int32_t capacity,
                                      int32_t* written);
MH_API int32_t mh_plugin_state_import(void* handle,
                                      int32_t kind,
                                      const uint8_t* data,
                                      int32_t len);
MH_API int32_t mh_plugin_take_edited(void* handle);

MH_API int32_t mh_plugin_editor_open(void* manager, void* handle);
MH_API int32_t mh_plugin_editor_close(void* manager, void* handle);
MH_API int32_t mh_plugin_editor_is_open(void* handle, int32_t* out_open);

#ifdef __cplusplus
}
#endif

#endif
