/*
 * MicHost virtual microphone - shared control ABI.
 *
 * This header is intentionally free of WDK types so the same definitions can
 * be used by the kernel driver, user-mode MicHost, and Rust bindings.
 *
 * The data path is a named section.  The driver creates the section on
 * OPEN_WRITER and returns its bare name (no "\BaseNamedObjects\" prefix) from
 * GET_SECTION_INFO.  The section begins with MH_VMIC_RING followed by
 * ring_frames * channels interleaved float32 samples.
 *
 * Single-writer contract:
 *   MicHost writes write_index, overruns and last_write_qpc.
 *   The driver writes read_index, underruns and qpc_frequency.
 *   writer_active is owned by the driver.
 */
#ifndef MH_VMIC_H
#define MH_VMIC_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MH_VMIC_DEVICE_PATH             "\\\\.\\MicHostVirtualMic"
#define MH_VMIC_DEVICE_NAME             L"\\\\.\\MicHostVirtualMic"

#define MH_VMIC_MAGIC                   0x4D48564Du  /* 'MHVM' */
#define MH_VMIC_VERSION                 1u

#define MH_VMIC_FILE_DEVICE_UNKNOWN     0x00000022u
#define MH_VMIC_METHOD_BUFFERED         0u
#define MH_VMIC_FILE_ANY_ACCESS         0u

#ifndef FILE_DEVICE_UNKNOWN
#define FILE_DEVICE_UNKNOWN             MH_VMIC_FILE_DEVICE_UNKNOWN
#endif
#ifndef METHOD_BUFFERED
#define METHOD_BUFFERED                 MH_VMIC_METHOD_BUFFERED
#endif
#ifndef FILE_ANY_ACCESS
#define FILE_ANY_ACCESS                 MH_VMIC_FILE_ANY_ACCESS
#endif

#if !defined(CTL_CODE) || defined(MH_VMIC_FORCE_CTL_CODE)
#undef CTL_CODE
#define MH_VMIC_CTL_CODE(DeviceType, Function, Method, Access) \
    (((DeviceType) << 16) | ((Access) << 14) | ((Function) << 2) | (Method))
#else
#define MH_VMIC_CTL_CODE(DeviceType, Function, Method, Access) \
    CTL_CODE((DeviceType), (Function), (Method), (Access))
#endif

#define MH_VMIC_IOCTL_GET_VERSION       MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define MH_VMIC_IOCTL_OPEN_WRITER       MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define MH_VMIC_IOCTL_CLOSE_WRITER      MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define MH_VMIC_IOCTL_GET_STATS         MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define MH_VMIC_IOCTL_SET_PACKET_FRAMES MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define MH_VMIC_IOCTL_GET_SECTION_INFO  MH_VMIC_CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_ANY_ACCESS)

#define MH_VMIC_DEFAULT_SAMPLE_RATE     48000u
#define MH_VMIC_DEFAULT_CHANNELS        2u
#define MH_VMIC_DEFAULT_PACKET_FRAMES   480u
#define MH_VMIC_BYTES_PER_SAMPLE        4u      /* float32 */
#define MH_VMIC_MIN_PACKET_FRAMES       16u
#define MH_VMIC_MAX_PACKET_FRAMES       16384u
#define MH_VMIC_MIN_RING_FRAMES         2048u   /* max(4 * packet, 2048) */
#define MH_VMIC_MAX_RING_FRAMES         (1u << 20)

#define MH_VMIC_SECTION_NAME_PREFIX     "MicHostVirtualMic-"
#define MH_VMIC_SECTION_NAME_CHARS      64u

#pragma pack(push, 8)
typedef struct MH_VMIC_RING
{
    uint32_t magic;                 /* MH_VMIC_MAGIC */
    uint32_t version;               /* MH_VMIC_VERSION */
    uint32_t sample_rate;
    uint32_t channels;
    uint32_t bytes_per_sample;      /* 4 = float32 */
    uint32_t packet_frames;         /* driver packet size */
    uint32_t ring_frames;           /* capacity in frames, power of two */
    uint32_t writer_active;         /* 1 while MicHost owns the writer */

    uint64_t write_index;           /* monotonic frames written (host) */
    uint64_t read_index;            /* monotonic frames consumed (driver) */
    uint64_t overruns;              /* host found ring full (host) */
    uint64_t underruns;             /* driver found ring empty (driver) */
    uint64_t qpc_frequency;         /* driver QPC frequency (driver) */
    uint64_t last_write_qpc;        /* host QPC of last write (host) */

    /* interleaved float32 data follows: ring_frames * channels samples */
} MH_VMIC_RING;
#pragma pack(pop)

#define MH_VMIC_RING_DATA(ring) \
    ((float *)((unsigned char *)(ring) + sizeof(MH_VMIC_RING)))
#define MH_VMIC_RING_BYTES(ring_frames, channel_count) \
    ((size_t)(ring_frames) * (size_t)(channel_count) * sizeof(float))
#define MH_VMIC_SECTION_BYTES(ring_frames, channel_count) \
    (sizeof(MH_VMIC_RING) + MH_VMIC_RING_BYTES((ring_frames), (channel_count)))

typedef struct MH_VMIC_OPEN_WRITER
{
    uint32_t SampleRate;
    uint32_t Channels;
    uint32_t PacketFrames;
} MH_VMIC_OPEN_WRITER;

typedef struct MH_VMIC_SET_PACKET_FRAMES
{
    uint32_t PacketFrames;
} MH_VMIC_SET_PACKET_FRAMES;

typedef struct MH_VMIC_SECTION_INFO
{
    uint32_t RingFrames;
    uint32_t PacketFrames;
    uint32_t SampleRate;
    uint32_t Channels;
    uint32_t NameBytes;             /* UTF-16LE bytes, no NUL */
    uint32_t Reserved;              /* must be zero */
    /* UTF-16LE section name follows, NameBytes bytes, no NUL */
} MH_VMIC_SECTION_INFO;

typedef struct MH_VMIC_STATS
{
    uint32_t ConsumerActive;        /* 1 while a WaveRT stream is running */
    uint32_t Reserved;              /* must be zero */
    uint64_t Underruns;
    uint64_t ReadIndex;
    uint64_t QpcFrequency;
} MH_VMIC_STATS;

/* C99 compile-time checks. */
#define MH_VMIC_STATIC_ASSERT(condition, tag) \
    typedef char mh_vmic_static_assert_##tag[(condition) ? 1 : -1]

MH_VMIC_STATIC_ASSERT(sizeof(MH_VMIC_RING) == 80, ring_size_is_80_bytes);
MH_VMIC_STATIC_ASSERT(offsetof(MH_VMIC_RING, magic) == 0, ring_magic_offset);
MH_VMIC_STATIC_ASSERT(offsetof(MH_VMIC_RING, writer_active) == 28, ring_writer_active_offset);
MH_VMIC_STATIC_ASSERT(offsetof(MH_VMIC_RING, write_index) == 32, ring_write_index_offset);
MH_VMIC_STATIC_ASSERT(offsetof(MH_VMIC_RING, last_write_qpc) == 72, ring_last_write_qpc_offset);
MH_VMIC_STATIC_ASSERT(sizeof(MH_VMIC_OPEN_WRITER) == 12, open_writer_size);
MH_VMIC_STATIC_ASSERT(sizeof(MH_VMIC_SET_PACKET_FRAMES) == 4, set_packet_frames_size);
MH_VMIC_STATIC_ASSERT(sizeof(MH_VMIC_SECTION_INFO) == 24, section_info_size);
MH_VMIC_STATIC_ASSERT(sizeof(MH_VMIC_STATS) == 32, stats_size);
#if defined(__cplusplus)
#define MH_VMIC_CPP_STATIC_ASSERT(condition, tag) static_assert((condition), #tag)
MH_VMIC_CPP_STATIC_ASSERT(sizeof(MH_VMIC_RING) == 80, ring_size_is_80_bytes);
MH_VMIC_CPP_STATIC_ASSERT(offsetof(MH_VMIC_RING, write_index) == 32, ring_write_index_offset);
MH_VMIC_CPP_STATIC_ASSERT(sizeof(MH_VMIC_OPEN_WRITER) == 12, open_writer_size);
MH_VMIC_CPP_STATIC_ASSERT(sizeof(MH_VMIC_SECTION_INFO) == 24, section_info_size);
MH_VMIC_CPP_STATIC_ASSERT(sizeof(MH_VMIC_STATS) == 32, stats_size);
#endif
#undef MH_VMIC_STATIC_ASSERT

/* Ring capacity rule: max(4 * packet_frames, 2048), rounded up to a power of two. */
static __inline uint32_t MhVmicRingFramesForPacket(uint32_t packet_frames)
{
    uint32_t frames = packet_frames * 4u;
    uint32_t power = MH_VMIC_MIN_RING_FRAMES;

    if (frames < MH_VMIC_MIN_RING_FRAMES)
    {
        frames = MH_VMIC_MIN_RING_FRAMES;
    }
    while (power < frames && power < MH_VMIC_MAX_RING_FRAMES)
    {
        power <<= 1u;
    }
    return power;
}

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* MH_VMIC_H */
