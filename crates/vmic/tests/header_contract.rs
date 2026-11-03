//! Textual drift guard for the C driver header.
//!
//! The kernel driver and the host crate are separate programs with one shared
//! ABI. A Rust unit test cannot link the WDK header, so this test checks the
//! parts of `driver/inc/mh_vmic.h` that both sides must agree on. Change both
//! files together; the real compile-time checks live in the WDK build.

use michost_vmic::{
    BYTES_PER_SAMPLE, DEFAULT_FORMAT, MAGIC, MAX_PACKET_FRAMES, MIN_PACKET_FRAMES, RING_FRAMES_MAX,
    RING_FRAMES_MIN, VERSION,
};

const HEADER: &str = include_str!("../../../driver/inc/mh_vmic.h");

fn assert_in_order(header: &str, needles: &[&str]) {
    let mut offset = 0;
    for needle in needles {
        let found = header[offset..]
            .find(needle)
            .unwrap_or_else(|| panic!("header is missing or reorders `{needle}`"));
        offset += found + needle.len();
    }
}

#[test]
fn ring_header_matches_rust_layout() {
    assert!(HEADER.contains("#define MH_VMIC_MAGIC                   0x4D48564Du"));
    assert!(HEADER.contains("#define MH_VMIC_VERSION                 1u"));
    assert!(HEADER.contains("sizeof(MH_VMIC_RING) == 80"));
    assert!(HEADER.contains("offsetof(MH_VMIC_RING, write_index) == 32"));
    assert!(HEADER.contains("offsetof(MH_VMIC_RING, last_write_qpc) == 72"));
    assert_eq!(MAGIC, 0x4D48_564D);
    assert_eq!(VERSION, 1);

    assert_in_order(
        HEADER,
        &[
            "uint32_t magic;",
            "uint32_t version;",
            "uint32_t sample_rate;",
            "uint32_t channels;",
            "uint32_t bytes_per_sample;",
            "uint32_t packet_frames;",
            "uint32_t ring_frames;",
            "uint32_t writer_active;",
            "uint64_t write_index;",
            "uint64_t read_index;",
            "uint64_t overruns;",
            "uint64_t underruns;",
            "uint64_t qpc_frequency;",
            "uint64_t last_write_qpc;",
        ],
    );
    assert_eq!(BYTES_PER_SAMPLE, 4);
}

#[test]
fn control_structs_and_bounds_match() {
    assert!(HEADER.contains("sizeof(MH_VMIC_OPEN_WRITER) == 12"));
    assert!(HEADER.contains("sizeof(MH_VMIC_SECTION_INFO) == 24"));
    assert!(HEADER.contains("sizeof(MH_VMIC_STATS) == 32"));
    assert!(HEADER.contains("MH_VMIC_IOCTL_GET_VERSION"));
    assert!(HEADER.contains("MH_VMIC_IOCTL_GET_SECTION_INFO"));
    assert!(HEADER.contains("MH_VMIC_IOCTL_SET_PACKET_FRAMES"));

    assert_in_order(
        HEADER,
        &[
            "uint32_t RingFrames;",
            "uint32_t PacketFrames;",
            "uint32_t SampleRate;",
            "uint32_t Channels;",
            "uint32_t NameBytes;",
            "uint32_t Reserved;",
        ],
    );

    assert!(HEADER.contains("#define MH_VMIC_MIN_PACKET_FRAMES       16u"));
    assert!(HEADER.contains("#define MH_VMIC_MAX_PACKET_FRAMES       16384u"));
    assert!(HEADER.contains("#define MH_VMIC_MIN_RING_FRAMES         2048u"));
    assert_eq!(MIN_PACKET_FRAMES, 16);
    assert_eq!(MAX_PACKET_FRAMES, 16_384);
    assert_eq!(RING_FRAMES_MIN, 2_048);
    assert_eq!(RING_FRAMES_MAX, 1_048_576);
    assert_eq!(DEFAULT_FORMAT.packet_frames, 480);
    assert_eq!(DEFAULT_FORMAT.sample_rate, 48_000);
}
