use ffmpeg::{ChannelLayout, codec as avcodec, format as avformat, software::resampling};
use std::path::Path;
const DECODE_SAMPLE_RATE: u32 = 48_000;
const WHISPER_SAMPLE_RATE: u32 = 16_000;
fn write_wav_16k_mono(mixed_samples: &[f32], output_path: &Path) -> Result<(), String> {
    write_wav_16k_mono_chunked(mixed_samples, output_path, 16_384)
}
fn write_wav_16k_mono_chunked(
    mixed_samples: &[f32],
    output_path: &Path,
    default_pcm_chunk: usize,
) -> Result<(), String> {
    let mut output =
        avformat::output(&output_path).map_err(|e| format!("Failed to create output file: {e}"))?;

    let codec = avcodec::encoder::find_by_name("pcm_s16le")
        .ok_or_else(|| "PCM encoder not found".to_string())?;

    let mut encoder = avcodec::Context::new()
        .encoder()
        .audio()
        .map_err(|e| format!("Failed to create encoder: {e}"))?;

    encoder.set_rate(WHISPER_SAMPLE_RATE as i32);
    let channel_layout = ChannelLayout::MONO;
    encoder.set_channel_layout(channel_layout);
    encoder.set_format(avformat::Sample::I16(avformat::sample::Type::Packed));

    let mut encoder = encoder
        .open_as(codec)
        .map_err(|e| format!("Failed to open encoder: {e}"))?;

    let mut stream = output
        .add_stream(codec)
        .map_err(|e| format!("Failed to add stream: {e}"))?;
    stream.set_parameters(&encoder);

    output
        .write_header()
        .map_err(|e| format!("Failed to write header: {e}"))?;

    let mut resampler = resampling::Context::get(
        avformat::Sample::F32(avformat::sample::Type::Packed),
        channel_layout,
        DECODE_SAMPLE_RATE,
        avformat::Sample::I16(avformat::sample::Type::Packed),
        channel_layout,
        WHISPER_SAMPLE_RATE,
    )
    .map_err(|e| format!("Failed to create resampler: {e}"))?;

    let frame_size = encoder.frame_size() as usize;
    let frame_size = if frame_size == 0 {
        default_pcm_chunk
    } else {
        frame_size
    };

    for chunk in mixed_samples.chunks(frame_size) {
        let mut input_frame = ffmpeg::frame::Audio::new(
            avformat::Sample::F32(avformat::sample::Type::Packed),
            chunk.len(),
            channel_layout,
        );
        input_frame.set_rate(DECODE_SAMPLE_RATE);

        // SAFETY: reinterpreting the f32 chunk as its little-endian bytes for
        // the frame copy, exactly as the Tauri encoder does.
        let bytes = unsafe {
            std::slice::from_raw_parts(chunk.as_ptr() as *const u8, std::mem::size_of_val(chunk))
        };
        input_frame.data_mut(0)[0..bytes.len()].copy_from_slice(bytes);

        let mut output_frame = ffmpeg::frame::Audio::new(
            avformat::Sample::I16(avformat::sample::Type::Packed),
            frame_size,
            ChannelLayout::MONO,
        );
        output_frame.set_rate(WHISPER_SAMPLE_RATE);

        if let Err(error) = resampler.run(&input_frame, &mut output_frame) {
            tracing::error!("Failed to resample chunk: {error}");
            continue;
        }

        if let Err(error) = encoder.send_frame(&output_frame) {
            tracing::error!("Failed to send frame to encoder: {error}");
            continue;
        }

        loop {
            let mut packet = ffmpeg::Packet::empty();
            if encoder.receive_packet(&mut packet).is_err() {
                break;
            }
            if let Err(error) = packet.write_interleaved(&mut output) {
                tracing::error!("Failed to write packet: {error}");
            }
        }
    }

    encoder
        .send_eof()
        .map_err(|e| format!("Failed to send EOF: {e}"))?;

    loop {
        let mut packet = ffmpeg::Packet::empty();
        if encoder.receive_packet(&mut packet).is_err() {
            break;
        }
        packet
            .write_interleaved(&mut output)
            .map_err(|e| format!("Failed to write final packet: {e}"))?;
    }

    output
        .write_trailer()
        .map_err(|e| format!("Failed to write trailer: {e}"))
}
fn main() {}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_pcm_chunking_preserves_legacy_wav_bytes() {
        ffmpeg::init().unwrap();
        let directory = tempfile::tempdir().unwrap();
        for len in [
            0, 1, 2, 3, 15, 16, 17, 1023, 1024, 1025, 16383, 16384, 16385, 32769,
        ] {
            let mut state = 0x1947_cdef_u32;
            let samples: Vec<f32> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as i32 as f32 / i32::MAX as f32 * 0.8
                })
                .collect();
            let reference = directory.path().join(format!("reference-{len}.wav"));
            let actual = directory.path().join(format!("actual-{len}.wav"));
            write_wav_16k_mono_chunked(&samples, &reference, 1024).unwrap();
            write_wav_16k_mono(&samples, &actual).unwrap();
            assert_eq!(
                std::fs::read(&actual).unwrap(),
                std::fs::read(&reference).unwrap(),
                "{len} samples"
            );
        }
    }
}
