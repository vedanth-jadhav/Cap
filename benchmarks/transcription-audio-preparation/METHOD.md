# Transcription preparation benchmark

The native probe preserves the eight audio-preparation functions from upstream `f6046f1ec82314b0aa887b76bc1ebf8bc77e3579`, with two runtime switches: normalize in the caller or in cap-audio; use 1024 or 16384 PCM input samples per frame. Decoder, assembly, offsets, normalization arithmetic, resampling and WAV writing remain unchanged. No inference, model download, GPU renderer or UI is involved. Measurements cover metadata read through complete WAV preparation only, not overall caption generation.

Hardware: Linux x86_64, AMD EPYC 9V74 host, 2-CPU quota; process affinity CPUs 0 and 1, native FFmpeg 7.1.5; Rust 1.88.0 for the native preparation probe. GPUI itself uses stable Rust 1.99.1, so a separate cross-profile parity test uses that toolchain. The dev-layout reproduction builds the caller at opt-level 0 and cap-audio/cap-editor at 2, mirroring GPUI's documented optimization boundaries. It retains root release thin LTO and is an isolated reproduction, not a timed GPUI executable. Shipping-style measurements use optimized caller and libraries; do not apply the development-only normalization gain to shipping builds.

Five fresh processes per mode and workload, alternating mode order on successive repetitions, run serially without concurrent compilation. Inputs are 1-hour AAC 48kHz stereo mic; independent mic plus system audio; and a 10-second mic control. Display metadata comes from a synthetic 320x180, 1fps H.264 track. Every complete WAV is hashed after timing and required to match the baseline byte for byte. The comparison driver deletes only the WAV it just generated after hashing and retains logs and samples.

Place `transcription-preparation-probe.rs` in `crates/editor/examples/` on the performance branch, then use the root workspace after documented native setup. Set CARGO_BUILD_JOBS=2.

```sh
python3 create-fixtures.py /tmp/transcription-fixtures
cargo rustc --locked -p cap-editor --release --example transcription-preparation-probe --config profile.release.package.cap-editor.opt-level=2 --config profile.release.package.cap-audio.opt-level=2 -- -C opt-level=0 -C codegen-units=256
python3 run-comparison.py target/release/examples/transcription-preparation-probe /tmp/transcription-fixtures /tmp/transcription-dev-results --modes baseline core core-batch
cargo build --locked -p cap-editor --release --example transcription-preparation-probe --config profile.release.package.cap-editor.opt-level=3 --config profile.release.package.cap-audio.opt-level=3
python3 run-comparison.py target/release/examples/transcription-preparation-probe /tmp/transcription-fixtures /tmp/transcription-release-results
```

`baseline` uses the original local normalizer and 1024 samples, `core` moves only normalization to cap-audio, `batch` changes only PCM batching, and `core-batch` enables both production changes. The fixture generator is an equivalent synthetic recipe; AAC output hashes may vary with FFmpeg builds, while baseline/candidate equality is checked locally within each run.

The changed writer is also checked at empty, sub-resampler-delay, legacy and new chunk boundaries, including partial final frames. No resampler-flush, timestamp, channel layout or transcription semantics changes are included.

Correctness checks independent of the UI binary:

```sh
cargo test --locked -p cap-audio --lib
# Copy transcription-pcm-native-tests.rs to crates/audio/examples/ first.
cargo test --locked -p cap-audio --example transcription-pcm-native-tests
rustc +stable --edition 2024 --crate-name normalization_native --crate-type rlib -C opt-level=2 crates/audio/src/normalization.rs -o /tmp/libnormalization_native.rlib
rustc +stable --edition 2024 --test -C opt-level=0 profile-parity-tests.rs --extern normalization_native=/tmp/libnormalization_native.rlib -o /tmp/normalization-parity
/tmp/normalization-parity
```

The full GPUI `cargo check --locked -p cap-desktop-gpui --bin cap-gpui` passes after applying the current official `scripts/prepare-gpui-dependency.sh` patch stack. An initial E0599 was due to older local patches, not an upstream source defect. The full UI test suite and ASR inference were not run; the exact production writer and test were executed in the native cap-audio example. See gpui-check.txt for the completed compile result.

Related open work checked before publication: #2747 changes which timeline ranges reach transcription (and retains these normalization/WAV helpers), #2814 sizes Whisper inference threads, and #3344 stages model downloads. This change preserves timeline selection and inference scheduling and does not duplicate those fixes.
